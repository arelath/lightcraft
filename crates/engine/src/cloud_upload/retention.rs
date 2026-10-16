//! Explicit orphan reclamation. Every retained operation is a protection root,
//! including superseded content, conflicts and tombstones. This is not history GC.
use super::*;
use crate::assets::s3::ObjectStat;
use lightcraft_catalog::replication::{Clock, Mutation};

mod audit;
#[cfg(test)]
mod tests;
const MAX_KEYS: usize = 20_000;
const MAX_METADATA: u64 = 256 << 20;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    key: String,
    stat: ObjectStat,
}

fn digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
}
/// Deliberately narrow: unknown/future layouts are retained.
fn media_hash<'a>(prefix: &str, key: &'a str) -> Option<&'a str> {
    let tail = key.strip_prefix(prefix)?.strip_prefix('/')?;
    let parts: Vec<_> = tail.split('/').collect();
    match parts.as_slice() {
        ["originals", hash] if digest(hash) => Some(hash),
        ["previews", source, "v1", file] if digest(source) => file.strip_suffix(".lcsp").filter(|hash| digest(hash)),
        ["thumbs", source, "v1", recipe, parameters, edge, file]
            if digest(source)
                && recipe.len() == 16
                && parameters.len() == 32
                && recipe.bytes().chain(parameters.bytes()).all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
                && edge.parse::<u32>().is_ok_and(|edge| (1..=1024).contains(&edge)) =>
        {
            file.strip_suffix(".png").filter(|hash| digest(hash))
        }
        // Supporting XMP is retained even if an older writer omitted its reference.
        _ => None,
    }
}
fn modified(value: &str) -> Result<i64> {
    let bad = || error("unrecognized object Last-Modified; no reclamation applied");
    let parts: Vec<_> = value.split_ascii_whitespace().collect();
    let [_, day, month, year, clock, "GMT"] = parts.as_slice() else {
        return Err(bad());
    };
    let month = match *month {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return Err(bad()),
    };
    let date = time::Date::from_calendar_date(
        year.parse().map_err(|_| bad())?,
        time::Month::try_from(month).map_err(|_| bad())?,
        day.parse().map_err(|_| bad())?,
    )
    .map_err(|_| bad())?;
    let clock: Vec<_> = clock.split(':').collect();
    let [hour, minute, second] = clock.as_slice() else {
        return Err(bad());
    };
    Ok(date
        .with_hms(hour.parse().map_err(|_| bad())?, minute.parse().map_err(|_| bad())?, second.parse().map_err(|_| bad())?)
        .map_err(|_| bad())?
        .assume_utc()
        .unix_timestamp())
}
fn list(connection: &S3Connection, cancel: &AtomicBool) -> Result<BTreeSet<String>> {
    let mut request = crate::cloud_append::listing::ListRequest {
        prefix: format!("{}/", connection.prefix),
        delimiter: false,
        continuation: None,
        start_after: None,
    };
    let mut seen = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for _ in 0..64 {
        if cancel.load(Ordering::Relaxed) {
            return Err(error("reclamation cancelled; remaining objects retained"));
        }
        let page = connection.list_objects_v2(&request, cancel).map_err(|e| error(e.to_string()))?;
        if page.keys.iter().any(|key| !keys.insert(key.clone())) || keys.len() > MAX_KEYS {
            return Err(error("retention inventory exceeds 20,000 objects or repeats keys; no deletion applied"));
        }
        let Some(next) = page.next else { return Ok(keys) };
        if !seen.insert(next.clone()) {
            return Err(error("retention listing repeated a continuation token; no deletion applied"));
        }
        request.continuation = Some(next);
    }
    Err(error("retention listing exceeded its page budget; no deletion applied"))
}
fn local_references(root: &Path, audit: &mut audit::Audit, cancel: &AtomicBool) -> Result<()> {
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    let mut entries = 0usize;
    let mut bytes = 0u64;
    while let Some((path, depth)) = pending.pop() {
        if cancel.load(Ordering::Relaxed) {
            return Err(error("retention cancelled; local staged data retained"));
        }
        let meta = std::fs::symlink_metadata(&path).map_err(io)?;
        entries = entries.saturating_add(1);
        if meta.file_type().is_symlink() || depth > 12 || entries > 100_000 {
            return Err(error("redirected or oversized managed storage scan; no reclamation applied"));
        }
        if meta.is_dir() {
            for entry in std::fs::read_dir(path).map_err(io)? {
                if pending.len().saturating_add(entries) >= 100_000 {
                    return Err(error("managed storage inventory exceeds 100,000 entries; nothing deleted"));
                }
                pending.push((entry.map_err(io)?.path(), depth + 1));
            }
        } else if path.extension().is_some_and(|ext| ext == "json") {
            bytes = bytes.saturating_add(meta.len());
            if bytes > MAX_METADATA || meta.len() > 64 << 20 {
                return Err(error("local retention metadata exceeds 256 MiB aggregate / 64 MiB per file; no reclamation applied"));
            }
            let mut data = Vec::new();
            std::fs::File::open(path).map_err(io)?.take((64 << 20) + 1).read_to_end(&mut data).map_err(io)?;
            if data.len() > 64 << 20 {
                return Err(error("managed metadata grew during retention scan; retry preview"));
            }
            audit.references(
                &serde_json::from_slice(&data).map_err(|_| error("unreadable managed manifest or receipt; repair it before reclamation"))?,
            )?;
        }
    }
    Ok(())
}

fn plan(connection: &S3Connection, root: &Path, local: &Value, days: u64, cancel: &AtomicBool, task: &TaskHandle) -> Result<Value> {
    let keys = list(connection, cancel)?;
    let mut audit = audit::Audit::default();
    audit.references(local)?;
    audit.local_clocks(&connection.prefix, local)?;
    local_references(root, &mut audit, cancel)?;
    let mut evidence = BTreeMap::new();
    let mut total = 0u64;
    for key in &keys {
        if media_hash(&connection.prefix, key).is_some()
            || key.strip_prefix(&format!("{}/", connection.prefix)).is_some_and(|tail| tail.starts_with("associated/"))
        {
            continue;
        }
        task.detail("Checking every retained metadata object and historical media reference");
        let reply = connection
            .get_small(key, 64 << 20, cancel)
            .map_err(|e| error(e.to_string()))?
            .ok_or_else(|| error("metadata disappeared during inventory; retry preview"))?;
        total = total.saturating_add(reply.bytes.len() as u64);
        if total > MAX_METADATA {
            return Err(error("remote retention audit exceeds 256 MiB; no reclamation applied"));
        }
        audit.record(&connection.prefix, key, &reply.bytes)?;
        evidence.insert(key.clone(), sha256(&reply.bytes).0);
    }
    audit.complete(&keys)?;
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let mut candidates = Vec::new();
    let mut retained = BTreeMap::<String, u64>::new();
    let mut candidate_bytes = 0u64;
    for (index, key) in keys.iter().enumerate() {
        let Some(hash) = media_hash(&connection.prefix, key) else {
            *retained.entry("historySupportingOrUnknown".into()).or_default() += 1;
            continue;
        };
        let reason = if audit.protects(key, hash) { Some("referencedIncludingHistoryAndLocalStaging") } else { None };
        if let Some(reason) = reason {
            *retained.entry(reason.into()).or_default() += 1;
            continue;
        }
        let stat = connection
            .stat_object(key, cancel)
            .map_err(|e| error(e.to_string()))?
            .ok_or_else(|| error("media disappeared during inventory; retry preview"))?;
        let age = now.checked_sub(modified(&stat.modified)?).filter(|age| *age >= 0);
        if stat.version.is_some() {
            *retained.entry("versioned".into()).or_default() += 1;
        } else if stat.size == 0 || stat.size > lightcraft_catalog::assets::MAX_BLOB_BYTES {
            *retained.entry("unrecognizedSize".into()).or_default() += 1;
        } else if age.is_none_or(|age| age < (days * 86400) as i64) {
            *retained.entry("youngerThanRetention".into()).or_default() += 1;
        } else {
            candidate_bytes = candidate_bytes.saturating_add(stat.size);
            candidates.push(Candidate { key: key.clone(), stat });
        }
        task.progress(index.saturating_add(1) as u64, keys.len() as u64);
    }
    if list(connection, cancel)? != keys {
        return Err(error("remote inventory changed during audit; retry preview; nothing deleted"));
    }
    let supported = connection.endpoint.as_ref().is_none_or(|endpoint| {
        lightcraft_fetch::http::Url::parse(endpoint).is_ok_and(|url| matches!(url.host.as_str(), "localhost" | "127.0.0.1" | "::1"))
    });
    let head = format!("{}/catalog/head.json", connection.prefix);
    if let Some(expected) = evidence.get(&head) {
        let actual = connection
            .get_small(&head, 16 << 10, cancel)
            .map_err(|e| error(e.to_string()))?
            .ok_or_else(|| error("head disappeared during audit; retry preview"))?;
        if &sha256(&actual.bytes).0 != expected {
            return Err(error("head changed during retention audit; retry preview; nothing deleted"));
        }
    }
    let binding = json!({"destination":connection.storage_snapshot(),"local":local,"metadata":evidence,"inventory":keys,"candidates":candidates,"olderThanDays":days});
    let token = sha256(&serde_json::to_vec(&binding).map_err(|e| error(e.to_string()))?).0;
    Ok(
        json!({"preview":token,"olderThanDays":days,"candidates":candidates,"candidateBytes":candidate_bytes,"retained":retained,"objects":keys.len(),"metadataBytes":total,
        "applySupported":supported,"publishedHistoryRetained":true,"policy":"Published photos, deleted photos, revisions, XMP and metadata history are retained indefinitely. Only unreferenced, unversioned orphan media can be reclaimed."}),
    )
}

pub fn start(s: &mut Session, p: &Value) -> Result<Value> {
    start_impl(s, p, None)
}
pub fn observed(s: &mut Session, p: &Value, observer: &mut dyn FnMut(u64, u64, &str) -> bool) -> Result<Value> {
    s.execute_fn("cloud.reclaim", |s| start_impl(s, p, Some(observer)))
}
fn start_impl(s: &mut Session, p: &Value, observer: Option<&mut dyn FnMut(u64, u64, &str) -> bool>) -> Result<Value> {
    let id = p.get("connection").and_then(Value::as_str).filter(|id| identifier(id)).ok_or_else(|| error("specify a reclamation connection"))?;
    let boolean = |name: &str, default| {
        p.get(name)
            .map(|value| value.as_bool().ok_or_else(|| error(format!("{name} must be true or false"))))
            .transpose()
            .map(|v| v.unwrap_or(default))
    };
    let apply = boolean("apply", false)?;
    let wait = boolean("wait", !crate::assets::is_cached_only())?;
    let days = p
        .get("olderThanDays")
        .map(|v| v.as_u64().filter(|v| (7..=3650).contains(v)).ok_or_else(|| error("olderThanDays must be between 7 and 3650")))
        .transpose()?
        .unwrap_or(30);
    let expected = p.get("preview").and_then(Value::as_str).unwrap_or_default().to_owned();
    if apply && (!digest(&expected) || !boolean("allWritersPausedAndPublished", false)?) {
        return Err(error(
            "apply requires the exact preview token and allWritersPausedAndPublished:true; every device must publish pending work and stop writing first",
        ));
    }
    if !s.cloud_paused || s.cloud_replication_jobs.running() || s.cloud_imports.preparing() > 0 || s.cloud_uploads.running() > 0 {
        return Err(error("pause cloud synchronization and wait for transfers/imports to finish before retention preview or apply"));
    }
    if s.cache_move.is_some() || s.interaction.is_some() || s.unsaved().is_some() {
        return Err(error("finish the current edit/cache move and save local work before retention audit"));
    }
    let state = s
        .catalog
        .sync_state()
        .filter(|state| state.connection == id)
        .ok_or_else(|| error("enable synchronization on this connection before retention audit"))?;
    if !state.pending.is_empty() {
        return Err(error("publish all pending metadata before reclamation preview"));
    }
    if wait && crate::assets::is_cached_only() {
        return Err(error("desktop reclamation runs in the background; omit wait"));
    }
    let local = json!({"assets":s.catalog.assets(),"state":state});
    let resolver = s.media.asset_resolver.clone().ok_or_else(|| error("open a persistent native library first"))?;
    resolver.validate_storage().map_err(|e| error(e.to_string()))?;
    let root = resolver.durable_root().ok_or_else(|| error("no durable managed storage"))?;
    let connection = s
        .library
        .as_mut()
        .ok_or_else(|| error("open a library first"))?
        .cloud_connections()?
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| error("configure the retention connection first"))?;
    if connection.prefix.is_empty() {
        return Err(error("orphan reclamation requires a dedicated nonempty library prefix"));
    }
    let coordinator = s.cloud_replication_jobs.coordinator.clone();
    let revision = resolver.connection_revision();
    let task =
        s.activity.start("uploadCleanup", if apply { "Reclaim Orphan Cloud Media" } else { "Preview Cloud Retention" }, crate::activity::Cancel::Yes);
    let task_id = task.id();
    let handle = task.handle();
    let cancel = task.cancel_flag().ok_or_else(|| error("reclamation cancellation unavailable"))?;
    let worker_cancel = cancel.clone();
    let worker_resolver = resolver.clone();
    let connection_id = id.to_owned();
    let (sender, receiver) = mpsc::sync_channel(2);
    std::thread::Builder::new().name("cloud-retention".into()).spawn(move||{
        let result=crate::guard::catch("cloud retention",||{
            let _permit=coordinator.acquire(&worker_cancel)?;
            let _storage=worker_resolver.durable_guard(&worker_cancel).map_err(|e|error(e.to_string()))?;
            let mut preview=plan(&connection,&root,&local,days,&worker_cancel,&handle)?;
            if apply {
                if preview["applySupported"]!=true {return Err(error("conditional orphan deletion is supported only on AWS S3 and local conformance fixtures; this endpoint is preview-only; no fallback deletion"));}
                if preview["preview"]!=expected {return Err(error("retention plan changed; review a new preview before applying; nothing deleted"));}
                let candidates:Vec<Candidate>=serde_json::from_value(preview["candidates"].clone()).map_err(|e|error(e.to_string()))?;
                let mut removed=Vec::new();
                for candidate in candidates {
                    if worker_resolver.connection_revision()!=revision||worker_cancel.load(Ordering::Relaxed){return Err(error("reclamation cancelled or destination changed; remaining objects retained; rerun preview to inspect progress"));}
                    worker_resolver.validate_storage().map_err(|e|error(e.to_string()))?;
                    connection.delete_unversioned(&candidate.key,&candidate.stat,&worker_cancel).map_err(|e|error(format!("reclamation stopped: {e}; remaining objects retained; rerun preview to inspect progress")))?;
                    removed.push(candidate.key);
                }
                preview["deleted"]=json!(removed);
            }
            preview["applied"]=json!(apply);Ok(preview)
        }).unwrap_or_else(|message|Err(error(message)));
        let _=sender.send(Event::Finished(result));
    }).map_err(io)?;
    s.cloud_uploads.pending.push(Pending {
        receiver,
        library: s.library_token(),
        resolver: resolver.clone(),
        connection_revision: revision,
        ids: BTreeMap::new(),
        verification_ids: BTreeSet::new(),
        item_outcomes: BTreeMap::new(),
        connections: BTreeSet::from([connection_id.clone()]),
        maintenance: Some(Maintenance::Retention { connection: connection_id, apply }),
        media_verified: BTreeSet::new(),
        guard: task,
        cancel: cancel.clone(),
    });
    if wait { wait_task(s, task_id, cancel, Arc::new(AtomicU64::new(0)), observer) } else { Ok(json!({"background":true,"task":task_id})) }
}
