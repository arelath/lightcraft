#[path = "support/cloud_upload.rs"]
mod support;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::Stdio;
use support::Fixture;

struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn until(mut ready: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !ready() {
        assert!(std::time::Instant::now() < deadline, "fixture operation timed out");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn xmp_and_external_revisions_publish_dependencies_with_stable_photo_identity() {
    let fixture = Fixture::new();
    let library = fixture.dir.join("revision-library");
    let (initial, source) = fixture.import(&library, false);
    std::fs::remove_file(source).unwrap();
    let (output, rows) = fixture.run(
        &library,
        &[
            ("cloud.upload", json!({})),
            ("photo.rate", json!({"ids":[1],"rating":4})),
            ("photo.saveMetadataToFile", json!({"ids":[1]})),
            ("cloud.upload", json!({})),
            ("photo.editExternal", json!({"ids":[1],"format":"tiff","bitDepth":16})),
        ],
    );
    assert!(output.status.success(), "{rows:?} {}", String::from_utf8_lossy(&output.stderr));
    let edit = &rows[4]["result"];
    let derived = edit["id"].as_u64().unwrap();
    let working = PathBuf::from(edit["path"].as_str().unwrap());
    let image = lightcraft_raster::Rgba8::from_fn(13, 9, |_, _| [60, 120, 180, 255]);
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &Default::default()).unwrap();
    std::fs::write(&working, &bytes).unwrap();
    let exported = fixture.dir.join("returned-export.png");
    let (output, rows) = fixture.run(
        &library,
        &[
            ("photo.reload", json!({"ids":[derived]})),
            ("cloud.upload", json!({})),
            ("cloud.uploadStatus", json!({})),
            ("app.export", json!({"ids":[derived],"path":exported,"format":"png"})),
            ("cloud.removeLocalCopy", json!({"ids":[derived]})),
        ],
    );
    assert!(output.status.success(), "{rows:?} {}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(rows[2]["result"]["uploads"].as_object().unwrap().len(), 4);
    assert!(rows[2]["result"]["uploads"].as_object().unwrap().values().all(|intent| intent["state"] == "synchronized"), "{rows:?}");
    assert_eq!((rows[3]["result"]["width"].as_u64(), rows[3]["result"]["height"].as_u64()), (Some(13), Some(9)));
    assert!(rows[4]["result"]["failed"].as_array().unwrap().is_empty(), "{rows:?}");
    assert_eq!(std::fs::read(&working).unwrap(), bytes);
    let state = fixture.state.lock().unwrap();
    assert_eq!(state.signature_errors, 0);
    let mut xmp_seen = false;
    let mut derived_identity = None;
    for entry in std::fs::read_dir(library.join("Managed/uploads")).unwrap() {
        let receipt: Value = serde_json::from_slice(&std::fs::read(entry.unwrap().path()).unwrap()).unwrap();
        let imported = &receipt["imported"];
        if imported["photo"]["id"] == 1 {
            assert_eq!(imported["photoId"], initial);
        } else {
            let identity = imported["photoId"].as_str().unwrap();
            if let Some(previous) = &derived_identity {
                assert_eq!(previous, identity);
            } else {
                derived_identity = Some(identity.to_owned());
            }
        }
        let asset = &imported["asset"];
        let documents = asset["associated"].as_object();
        xmp_seen |= documents.is_some_and(|documents| documents.contains_key("xmp"));
        for blob in [&asset["original"], &asset["smartPreview"], &asset["thumbnail"]]
            .into_iter()
            .chain(documents.into_iter().flat_map(|documents| documents.values()))
        {
            let key = blob["remote"]["key"].as_str().unwrap();
            assert_eq!(support::hash(state.objects.get(&format!("/photos/{key}")).unwrap()), blob["id"].as_str().unwrap());
        }
    }
    assert!(xmp_seen && derived_identity.is_some());
}

#[test]
fn unsupported_conditional_writes_preserve_staging_and_multipart_without_publishing() {
    for large in [false, true] {
        let fixture = Fixture::new();
        let library = fixture.dir.join("library");
        let (id, source) = fixture.import(&library, large);
        std::fs::remove_file(source).unwrap();
        fixture.state.lock().unwrap().unsupported_conditional = true;
        let (_, rows) = fixture.run(&library, &[("cloud.upload", json!({"imports":[id]})), ("cloud.uploadStatus", json!({}))]);
        assert_eq!(rows[0]["ok"], false);
        assert!(rows[0]["error"].as_str().unwrap().contains("If-Match"), "{rows:?}");
        assert_eq!(rows[1]["result"]["uploads"][&id]["state"], "failed");
        let receipt: Value = serde_json::from_slice(&std::fs::read(library.join("Managed/uploads").join(format!("{id}.json"))).unwrap()).unwrap();
        let original = receipt["imported"]["asset"]["original"]["id"].as_str().unwrap();
        assert!(library.join("Managed/blobs").join(original).is_file());
        assert_eq!(receipt["multipart"].as_object().unwrap().len(), usize::from(large));
        let state = fixture.state.lock().unwrap();
        assert!(!state.objects.contains_key("/photos/library/catalog/head.json"));
        assert_eq!(state.signature_errors, 0);
    }
}

#[test]
fn uploaded_original_removal_preserves_library_previews_remote_data_and_reacquires() {
    let fixture = Fixture::new();
    let library = fixture.dir.join("library");
    let (id, source) = fixture.import(&library, false);
    let original_bytes = std::fs::read(&source).unwrap();
    std::fs::remove_file(source).unwrap();
    let (_, rows) = fixture.run(&library, &[("cloud.removeLocalCopy", json!({"ids":[1]}))]);
    assert!(!rows[0]["result"]["failed"].as_array().unwrap().is_empty(), "unuploaded original remains durable: {rows:?}");
    let (output, rows) = fixture.run(&library, &[("cloud.upload", json!({"imports":[id]}))]);
    assert!(output.status.success(), "{rows:?}");
    let receipt: Value = serde_json::from_slice(&std::fs::read(library.join("Managed/uploads").join(format!("{id}.json"))).unwrap()).unwrap();
    let original = receipt["imported"]["asset"]["original"]["id"].as_str().unwrap();
    let preview = receipt["imported"]["asset"]["smartPreview"]["id"].as_str().unwrap();
    let objects = fixture.state.lock().unwrap().objects.clone();
    let working = library.join("Managed/working/awaiting-upload.tif");
    std::fs::create_dir_all(working.parent().unwrap()).unwrap();
    std::fs::write(&working, b"durable external revision").unwrap();
    let (_, rows) = fixture.run(
        &library,
        &[
            ("cloud.pin", json!({"ids":[1],"wait":true})),
            ("cloud.removeLocalCopy", json!({"ids":[1]})),
            ("cloud.unpin", json!({"ids":[1]})),
            ("cloud.removeLocalCopy", json!({"ids":[1]})),
            ("cloud.status", json!({"ids":[1]})),
        ],
    );
    assert!(rows.iter().all(|row| row["ok"] == true), "{rows:?}");
    assert_eq!(rows[1]["result"]["failed"].as_array().unwrap().len(), 1, "pins survive restart");
    assert_eq!(rows[3]["result"]["removed"], json!([original]));
    assert!(rows[3]["result"]["failed"].as_array().unwrap().is_empty());
    assert!(!library.join("Managed/blobs").join(original).exists());
    assert!(library.join("assets-cache").join(preview).is_file(), "editable proxy survives original removal");
    assert_eq!(std::fs::read(&working).unwrap(), b"durable external revision");
    assert_eq!(fixture.state.lock().unwrap().objects, objects, "local removal never mutates remote objects");
    let out = fixture.dir.join("restored.png");
    let (output, rows) = fixture.run(
        &library,
        &[
            ("cloud.fetch", json!({"ids":[1],"wait":true})),
            ("app.export", json!({"ids":[1],"path":out,"format":"original"})),
            ("photo.delete", json!({"ids":[1]})),
            ("library.emptyRecentlyDeleted", json!({})),
        ],
    );
    assert!(output.status.success(), "{rows:?}");
    assert_eq!(std::fs::read(out).unwrap(), original_bytes);
    assert_eq!(fixture.state.lock().unwrap().objects, objects, "library deletion retains immutable remote objects");
    assert_eq!(std::fs::read(working).unwrap(), b"durable external revision");
}

#[test]
fn legacy_synchronized_receipt_requires_explicit_remote_reverification_before_eviction() {
    let fixture = Fixture::new();
    let library = fixture.dir.join("library");
    let (id, _) = fixture.import(&library, false);
    let (_, rows) = fixture.run(&library, &[("cloud.upload", json!({"imports":[id]}))]);
    assert_eq!(rows[0]["ok"], true, "{rows:?}");
    let path = library.join("Managed/uploads").join(format!("{id}.json"));
    let mut receipt: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let original = receipt["imported"]["asset"]["original"]["id"].as_str().unwrap().to_owned();
    for field in ["original", "smartPreview", "thumbnail"] {
        let blob = receipt["imported"]["asset"][field]["id"].as_str().unwrap();
        let cached = library.join("assets-cache").join(blob);
        let staged = library.join("Managed/blobs").join(blob);
        if !staged.exists() {
            std::fs::copy(&cached, &staged).unwrap();
        }
    }
    receipt["version"] = json!(1);
    receipt.as_object_mut().unwrap().remove("storage");
    std::fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
    let (_, rows) = fixture.run(&library, &[("cloud.removeLocalCopy", json!({"ids":[1]}))]);
    assert_eq!(rows[0]["result"]["failed"].as_array().unwrap().len(), 1);
    assert!(library.join("Managed/blobs").join(&original).is_file());
    let (_, rows) = fixture.run(&library, &[("cloud.upload", json!({"imports":[id],"verify":true})), ("cloud.removeLocalCopy", json!({"ids":[1]}))]);
    assert!(rows.iter().all(|row| row["ok"] == true), "{rows:?}");
    assert_eq!(rows[1]["result"]["removed"], json!([original]));
    assert_eq!(serde_json::from_slice::<Value>(&std::fs::read(path).unwrap()).unwrap()["version"], 2);
    assert_eq!(
        fixture.state.lock().unwrap().requests.iter().filter(|(method, key)| method == "PUT" && key.ends_with("/catalog/head.json")).count(),
        1
    );
}

#[test]
fn process_crash_after_remote_publication_reconciles_before_local_acknowledgement() {
    let fixture = Fixture::new();
    let library = fixture.dir.join("library");
    let (id, source) = fixture.import(&library, false);
    std::fs::remove_file(source).unwrap();
    fixture.state.lock().unwrap().hold_head = true;
    let mut process = Child(
        fixture
            .command()
            .args(["run", "--library"])
            .arg(&library)
            .arg("cloud.upload")
            .arg(json!({"imports":[id]}).to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    until(|| fixture.state.lock().unwrap().objects.contains_key("/photos/library/catalog/head.json"));
    process.0.kill().unwrap();
    process.0.wait().unwrap();
    fixture.state.lock().unwrap().hold_head = false;
    let receipt: Value = serde_json::from_slice(&std::fs::read(library.join("Managed/uploads").join(format!("{id}.json"))).unwrap()).unwrap();
    assert_eq!(receipt["state"], "objectsVerified", "crash occurs before publication receipt and local acknowledgement");
    let (output, rows) = fixture.run(&library, &[("cloud.upload", json!({"imports":[id]})), ("cloud.uploadStatus", json!({}))]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(rows.iter().all(|row| row["ok"] == true), "{rows:?}");
    assert_eq!(rows[1]["result"]["uploads"][&id]["state"], "synchronized");
    let state = fixture.state.lock().unwrap();
    assert_eq!(state.requests.iter().filter(|(method, target)| method == "PUT" && target.ends_with("/catalog/head.json")).count(), 1);
    assert_eq!(state.signature_errors, 0);
}

#[test]
fn mcp_upload_emits_bytes_answers_ping_and_cancels_without_losing_the_original() {
    let fixture = Fixture::new();
    let library = fixture.dir.join("library");
    let (id, source) = fixture.import(&library, true);
    std::fs::remove_file(source).unwrap();
    fixture.state.lock().unwrap().hold_part = true;
    let mut process = Child(
        fixture
            .command()
            .args(["mcp", "--library"])
            .arg(&library)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut input = process.0.stdin.take().unwrap();
    let output_lines = BufReader::new(process.0.stdout.take().unwrap()).lines();
    let (sender, output) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in output_lines {
            let _ = sender.send(line);
        }
    });
    let send = |input: &mut std::process::ChildStdin, request: Value| {
        writeln!(input, "{request}").unwrap();
        input.flush().unwrap();
    };
    let read = || {
        let line = output.recv_timeout(std::time::Duration::from_secs(10)).expect("MCP response or byte progress within ten seconds").unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };
    send(
        &mut input,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"upload-recovery","version":"1"}}}),
    );
    assert_eq!(read()["id"], 1);
    send(
        &mut input,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"command_run","arguments":{"id":"cloud.upload","params":{"imports":[id]}},"_meta":{"progressToken":"upload-bytes"}}}),
    );
    until(|| fixture.state.lock().unwrap().requests.iter().any(|(method, target)| method == "PUT" && target.contains("partNumber=1&")));
    let mut progress = Vec::new();
    loop {
        let row = read();
        assert_ne!(row["id"], 2, "upload completed before progress: {row}");
        if row["method"] == "notifications/progress" {
            assert!(row["params"].get("total").is_none());
            let bytes = row["params"]["progress"].as_u64().unwrap();
            progress.push(bytes);
            if bytes > 0 {
                break;
            }
        }
    }
    send(&mut input, json!({"jsonrpc":"2.0","id":3,"method":"ping"}));
    loop {
        let row = read();
        if row["method"] == "notifications/progress" {
            assert_eq!(row["params"]["progressToken"], "upload-bytes");
            assert!(row["params"].get("total").is_none(), "verification and retry bytes have no fixed total");
            progress.push(row["params"]["progress"].as_u64().unwrap());
        }
        if row["id"] == 3 {
            break;
        }
    }
    assert!(progress.iter().any(|bytes| *bytes > 0), "MCP reports transferred bytes before completion");
    assert!(progress.windows(2).all(|pair| pair[0] <= pair[1]));
    send(&mut input, json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2,"reason":"test cancellation"}}));
    send(&mut input, json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"command_run","arguments":{"id":"cloud.uploadStatus"}}}));
    loop {
        let row = read();
        if row["id"] == 4 {
            let status = &row["result"]["structuredContent"];
            assert_eq!(status["uploads"][&id]["state"], "failed", "{row:?}");
            break;
        }
    }
    drop(input);
    assert!(process.0.wait().unwrap().success());
    fixture.state.lock().unwrap().hold_part = false;
    assert!(!fixture.state.lock().unwrap().objects.contains_key("/photos/library/catalog/head.json"));
    let (output, rows) = fixture.run(&library, &[("cloud.upload", json!({"imports":[id]}))]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(rows[0]["ok"], true, "{rows:?}");
    assert_eq!(fixture.state.lock().unwrap().created, 1);
}

#[test]
fn lost_part_object_and_head_responses_resume_once_without_the_source_file() {
    let fixture = Fixture::new();
    let library = fixture.dir.join("library");
    let (id, source) = fixture.import(&library, true);
    fixture.state.lock().unwrap().lost_part = true;
    let (_, rows) = fixture.run(&library, &[("cloud.upload", json!({"imports":[id]})), ("cloud.uploadStatus", json!({}))]);
    assert_eq!(rows[0]["ok"], false, "{rows:?}");
    assert_eq!(rows[1]["result"]["uploads"][&id]["state"], "failed");
    assert!(!fixture.state.lock().unwrap().objects.contains_key("/photos/library/catalog/head.json"));
    let path = library.join("Managed/uploads").join(format!("{id}.json"));
    let receipt: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(receipt["multipart"].as_object().unwrap().len(), 1);
    assert!(receipt["multipart"].as_object().unwrap().values().next().unwrap()["parts"].as_object().unwrap().is_empty());
    let original = receipt["imported"]["asset"]["original"]["id"].as_str().unwrap();
    assert!(library.join("Managed/blobs").join(original).is_file());
    std::fs::remove_file(source).unwrap();
    {
        let mut state = fixture.state.lock().unwrap();
        state.lost_put = true;
        state.lost_head = true;
    }
    let out = fixture.dir.join("recovered.png");
    let (output, rows) = fixture.run(
        &library,
        &[
            ("cloud.upload", json!({"imports":[id]})),
            ("cloud.uploadStatus", json!({})),
            ("develop.set", json!({"id":1,"control":"light.exposure","value":0.5})),
            ("app.export", json!({"ids":[1],"path":out,"format":"png","longEdge":0})),
        ],
    );
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(rows.iter().all(|row| row["ok"] == true), "{rows:?}");
    assert_eq!(rows[1]["result"]["uploads"][&id]["state"], "synchronized");
    assert_eq!(rows[3]["result"]["files"][0]["width"], 32);
    assert!(out.is_file());
    let state = fixture.state.lock().unwrap();
    assert_eq!(state.created, 1, "restart reuses its persisted multipart identity");
    assert_eq!(state.signature_errors, 0, "all methods, body digests, conditions and encoded queries must sign correctly");
    let heads = state.requests.iter().filter(|(method, key)| method == "PUT" && key.ends_with("/catalog/head.json")).count();
    assert_eq!(heads, 1, "a lost publication response does not duplicate its mutation");
    drop(state);
    let (_, rows) = fixture.run(&library, &[("cloud.upload", json!({"imports":[id]}))]);
    assert_eq!(rows[0]["ok"], true);
    assert_eq!(rows[0]["result"]["tasks"].as_array().unwrap().len(), 0);
}

#[test]
fn embedded_completion_error_retains_parts_and_explicit_abort_preserves_original() {
    let fixture = Fixture::new();
    let library = fixture.dir.join("library");
    let (id, _) = fixture.import(&library, true);
    fixture.state.lock().unwrap().embedded_error = true;
    let (_, rows) = fixture.run(&library, &[("cloud.upload", json!({"imports":[id]}))]);
    assert_eq!(rows[0]["ok"], false, "HTTP 200 with Error cannot publish: {rows:?}");
    assert!(!fixture.state.lock().unwrap().objects.contains_key("/photos/library/catalog/head.json"));
    let path = library.join("Managed/uploads").join(format!("{id}.json"));
    let receipt: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(receipt["multipart"].as_object().unwrap().values().next().unwrap()["parts"].as_object().unwrap().len(), 2);
    let original = receipt["imported"]["asset"]["original"]["id"].as_str().unwrap();
    let (_, rows) = fixture.run(
        &library,
        &[("cloud.abortUploads", json!({"imports":[id]})), ("cloud.uploadStatus", json!({})), ("cloud.removeLocalCopy", json!({"ids":[1]}))],
    );
    assert!(rows.iter().all(|row| row["ok"] == true), "{rows:?}");
    assert_eq!(rows[1]["result"]["uploads"][&id]["state"], "savedLocally");
    assert!(!rows[2]["result"]["failed"].as_array().unwrap().is_empty());
    assert!(library.join("Managed/blobs").join(original).is_file());
    let (output, rows) = fixture.run(&library, &[("cloud.upload", json!({"imports":[id]}))]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(rows[0]["ok"], true, "{rows:?}");
    assert_eq!(fixture.state.lock().unwrap().created, 2);
}

#[test]
fn one_failed_import_does_not_block_the_remaining_uploads() {
    let fixture = Fixture::new();
    let library = fixture.dir.join("library");
    let (large, _) = fixture.import(&library, true);
    let (small, _) = fixture.import(&library, false);
    fixture.state.lock().unwrap().embedded_error = true;
    let (_, rows) = fixture.run(&library, &[("cloud.upload", json!({})), ("cloud.uploadStatus", json!({}))]);
    assert_eq!(rows[0]["ok"], false, "the group reports the failed import");
    assert_eq!(rows[1]["result"]["uploads"][&large]["state"], "failed");
    assert_eq!(rows[1]["result"]["uploads"][&small]["state"], "synchronized", "{rows:?}");
    assert!(fixture.state.lock().unwrap().objects.contains_key("/photos/library/catalog/head.json"));
}

#[test]
fn rejected_credentials_leave_staged_originals_editable_and_retryable() {
    let fixture = Fixture::new();
    let library = fixture.dir.join("library");
    let (id, source) = fixture.import(&library, false);
    std::fs::remove_file(source).unwrap();
    fixture.state.lock().unwrap().unauthorized = true;
    let out = fixture.dir.join("offline.png");
    let (_, rows) = fixture.run(
        &library,
        &[
            ("cloud.upload", json!({"imports":[id]})),
            ("app.export", json!({"ids":[1],"path":out,"format":"png","longEdge":0})),
            ("cloud.uploadStatus", json!({})),
        ],
    );
    assert_eq!(rows[0]["ok"], false);
    assert_eq!(rows[1]["ok"], true);
    assert!(out.is_file());
    assert_eq!(rows[2]["result"]["uploads"][&id]["state"], "failed");
    fixture.state.lock().unwrap().unauthorized = false;
    let (_, rows) = fixture.run(&library, &[("cloud.upload", json!({"imports":[id]}))]);
    assert_eq!(rows[0]["ok"], true, "{rows:?}");
    assert_eq!(fixture.state.lock().unwrap().signature_errors, 0);
}

#[test]
fn expired_deleted_receipt_cleanup_previews_confirms_abort_and_preserves_durable_bytes() {
    let fixture = Fixture::new();
    let library = fixture.dir.join("library");
    let (id, _) = fixture.import(&library, true);
    fixture.state.lock().unwrap().embedded_error = true;
    let (_, rows) = fixture.run(&library, &[("cloud.upload", json!({"imports":[id]}))]);
    assert_eq!(rows[0]["ok"], false);
    let receipt_path = library.join("Managed/uploads").join(format!("{id}.json"));
    let receipt_bytes = std::fs::read(&receipt_path).unwrap();
    let receipt: Value = serde_json::from_slice(&receipt_bytes).unwrap();
    let original = library.join("Managed/blobs").join(receipt["imported"]["asset"]["original"]["id"].as_str().unwrap());
    let original_bytes = std::fs::read(&original).unwrap();
    let working = library.join("Managed/working/unsaved-editor.tif");
    std::fs::create_dir_all(working.parent().unwrap()).unwrap();
    std::fs::write(&working, b"pending external editor data").unwrap();
    let (_, rows) = fixture.run(
        &library,
        &[
            ("photo.delete", json!({"ids":[1]})),
            ("library.emptyRecentlyDeleted", json!({})),
            ("cloud.cleanupUploads", json!({"connection":"personal"})),
        ],
    );
    assert!(rows.iter().all(|row| row["ok"] == true), "{rows:?}");
    assert_eq!(rows[2]["result"]["candidates"].as_array().unwrap().len(), 0, "young receipts remain resumable");
    std::fs::File::options()
        .write(true)
        .open(&receipt_path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 86400)))
        .unwrap();
    let (output, rows) = fixture.run(&library, &[("cloud.cleanupUploads", json!({"connection":"personal"}))]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(rows[0]["result"]["applied"], false);
    assert_eq!(rows[0]["result"]["candidates"][0]["import"], id);
    assert_eq!(std::fs::read(&receipt_path).unwrap(), receipt_bytes, "preview does not rewrite receipt");
    assert!(!fixture.state.lock().unwrap().requests.iter().any(|(method, _)| method == "DELETE"));
    // Simulate a part still appearing after the first abort acknowledgement.
    fixture.state.lock().unwrap().defer_abort = 1;
    let (output, rows) = fixture.run(&library, &[("cloud.cleanupUploads", json!({"connection":"personal", "apply":true}))]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(rows[0]["result"]["results"][0]["aborted"], 1);
    let after: Value = serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
    assert!(after["multipart"].as_object().unwrap().is_empty());
    assert_eq!(after["imported"], receipt["imported"]);
    assert_eq!(std::fs::read(original).unwrap(), original_bytes);
    assert_eq!(std::fs::read(working).unwrap(), b"pending external editor data");
    let state = fixture.state.lock().unwrap();
    let requests = state.requests.iter().filter(|(_, target)| target.contains("uploadId=")).map(|(method, _)| method.as_str()).collect::<Vec<_>>();
    assert!(requests.ends_with(&["DELETE", "GET", "DELETE", "GET"]), "{requests:?}");
    assert_eq!(state.signature_errors, 0);
}

#[test]
fn cleanup_preserves_receipts_when_authorization_or_destination_is_wrong() {
    let fixture = Fixture::new();
    let library = fixture.dir.join("library");
    let (id, _) = fixture.import(&library, true);
    fixture.state.lock().unwrap().embedded_error = true;
    let (_, rows) = fixture.run(&library, &[("cloud.upload", json!({"imports":[id]}))]);
    assert_eq!(rows[0]["ok"], false);
    let path = library.join("Managed/uploads").join(format!("{id}.json"));
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 86400)))
        .unwrap();
    let saved = std::fs::read(&path).unwrap();
    fixture.state.lock().unwrap().unauthorized = true;
    let (_, rows) = fixture.run(&library, &[("cloud.cleanupUploads", json!({"connection":"personal","apply":true}))]);
    assert_eq!(rows[0]["ok"], false);
    assert_eq!(std::fs::read(&path).unwrap(), saved);
    fixture.state.lock().unwrap().unauthorized = false;
    let requests_before = fixture.state.lock().unwrap().requests.len();
    let (_, rows) = fixture.run(&library, &[
        ("cloud.configure", json!({"id":"personal","bucket":"other-photos","region":"us-east-1","prefix":"library","endpoint":fixture.endpoint,"profile":"acceptance"})),
        ("cloud.cleanupUploads", json!({"connection":"personal","apply":true})),
        ("cloud.upload", json!({"imports":[id]})),
    ]);
    assert_eq!(rows[0]["ok"], true);
    assert_eq!(rows[1]["ok"], false);
    assert_eq!(rows[2]["ok"], false);
    assert_eq!(fixture.state.lock().unwrap().requests.len(), requests_before, "old multipart identities cannot be sent to another bucket");
    assert_eq!(std::fs::read(path).unwrap(), saved);
}

#[test]
fn mcp_cleanup_answers_ping_and_cancellation_retains_the_uncertain_receipt() {
    let fixture = Fixture::new();
    let library = fixture.dir.join("library");
    let (id, _) = fixture.import(&library, true);
    fixture.state.lock().unwrap().embedded_error = true;
    let (_, rows) = fixture.run(&library, &[("cloud.upload", json!({"imports":[id]}))]);
    assert_eq!(rows[0]["ok"], false);
    let path = library.join("Managed/uploads").join(format!("{id}.json"));
    let saved = std::fs::read(&path).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 86400)))
        .unwrap();
    fixture.state.lock().unwrap().hold_abort = true;
    let mut process = Child(
        fixture
            .command()
            .args(["mcp", "--library"])
            .arg(&library)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut input = process.0.stdin.take().unwrap();
    let lines = BufReader::new(process.0.stdout.take().unwrap()).lines();
    let (sender, output) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in lines {
            let _ = sender.send(line);
        }
    });
    let send = |input: &mut std::process::ChildStdin, request: Value| {
        writeln!(input, "{request}").unwrap();
        input.flush().unwrap();
    };
    let read = || {
        serde_json::from_str::<Value>(
            &output.recv_timeout(std::time::Duration::from_secs(10)).expect("MCP cleanup response within ten seconds").unwrap(),
        )
        .unwrap()
    };
    send(
        &mut input,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"cleanup","version":"1"}}}),
    );
    assert_eq!(read()["id"], 1);
    send(
        &mut input,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"command_run","arguments":{"id":"cloud.cleanupUploads","params":{"connection":"personal","apply":true}},"_meta":{"progressToken":"cleanup"}}}),
    );
    until(|| fixture.state.lock().unwrap().requests.iter().any(|(method, target)| method == "DELETE" && target.contains("uploadId=")));
    send(&mut input, json!({"jsonrpc":"2.0","id":3,"method":"ping"}));
    loop {
        let row = read();
        assert_ne!(row["id"], 2, "cleanup completed before cancellation: {row}");
        if row["id"] == 3 {
            break;
        }
    }
    send(&mut input, json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2,"reason":"retain uncertain receipt"}}));
    send(&mut input, json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"command_run","arguments":{"id":"cloud.uploadStatus"}}}));
    loop {
        let row = read();
        if row["id"] == 4 {
            let reports = row["result"]["structuredContent"]["reports"].as_array().unwrap();
            assert!(reports.iter().any(|report| report["ok"] == false), "{row}");
            break;
        }
    }
    drop(input);
    assert!(process.0.wait().unwrap().success());
    assert_eq!(std::fs::read(&path).unwrap(), saved);
    fixture.state.lock().unwrap().hold_abort = false;
    let (output, rows) = fixture.run(&library, &[("cloud.cleanupUploads", json!({"connection":"personal","apply":true}))]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(rows[0]["result"]["results"][0]["aborted"], 1);
    assert_eq!(fixture.state.lock().unwrap().signature_errors, 0);
}
