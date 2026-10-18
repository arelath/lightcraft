//! Browser storage, asynchronous: OPFS (Origin Private File System) when the page can write it
//! from the main thread (`FileSystemFileHandle.createWritable`), else IndexedDB. Used from the
//! window and from render workers alike. Keys are `/`-separated paths (OPFS directories; plain
//! IndexedDB keys).

use js_sys::{Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemGetDirectoryOptions, FileSystemGetFileOptions, IdbDatabase};

const IDB_NAME: &str = "lightcraft";
const IDB_STORE: &str = "files";

#[derive(Clone)]
pub enum Backend {
    Opfs(FileSystemDirectoryHandle),
    Idb(IdbDatabase),
}

fn err(e: JsValue) -> String {
    let name = Reflect::get(&e, &"name".into()).ok().and_then(|v| v.as_string());
    let message = Reflect::get(&e, &"message".into()).ok().and_then(|v| v.as_string()).or_else(|| e.as_string()).unwrap_or_else(|| format!("{e:?}"));
    match name {
        Some(name) => format!("{name}: {message}"),
        None => message,
    }
}
fn missing(e: &JsValue) -> bool {
    Reflect::get(e, &"name".into()).ok().and_then(|name| name.as_string()).as_deref() == Some("NotFoundError")
}
fn failure(event: JsValue, fallback: &JsValue) -> JsValue {
    Reflect::get(&event, &"target".into())
        .ok()
        .and_then(|target| Reflect::get(&target, &"error".into()).ok())
        .filter(|value| !value.is_null() && !value.is_undefined())
        .or_else(|| Reflect::get(fallback, &"error".into()).ok().filter(|value| !value.is_null() && !value.is_undefined()))
        .unwrap_or(event)
}

/// `navigator.storage` in a window or a worker.
fn storage_manager() -> Option<web_sys::StorageManager> {
    let g = js_sys::global();
    let nav = Reflect::get(&g, &"navigator".into()).ok()?;
    let storage = Reflect::get(&nav, &"storage".into()).ok()?;
    (!storage.is_undefined()).then(|| storage.unchecked_into())
}

fn indexed_db() -> Option<web_sys::IdbFactory> {
    let f = Reflect::get(&js_sys::global(), &"indexedDB".into()).ok()?;
    (!f.is_undefined() && !f.is_null()).then(|| f.unchecked_into())
}

/// Can files be written through OPFS writable streams here?
fn opfs_writable() -> bool {
    let g = js_sys::global();
    Reflect::get(&g, &"FileSystemFileHandle".into())
        .ok()
        .filter(|c| !c.is_undefined())
        .and_then(|c| Reflect::get(&c, &"prototype".into()).ok())
        .and_then(|p| Reflect::get(&p, &"createWritable".into()).ok())
        .is_some_and(|f| f.is_function())
}

/// Resolve an IndexedDB request.
async fn idb_req(req: &web_sys::IdbRequest) -> Result<JsValue, String> {
    let r1 = req.clone();
    let p = js_sys::Promise::new(&mut |resolve, reject| {
        let r2 = r1.clone();
        let ok = Closure::once_into_js(move || {
            let _ = resolve.call1(&JsValue::NULL, &r2.result().unwrap_or(JsValue::UNDEFINED));
        });
        let failed = r1.clone();
        let fail = Closure::once_into_js(move |e: JsValue| {
            let _ = reject.call1(&JsValue::NULL, &failure(e, failed.as_ref()));
        });
        r1.set_onsuccess(Some(ok.unchecked_ref()));
        r1.set_onerror(Some(fail.unchecked_ref()));
    });
    JsFuture::from(p).await.map_err(err)
}

/// Wait for an IndexedDB transaction to commit.
async fn idb_done(tx: &web_sys::IdbTransaction) -> Result<(), String> {
    let t1 = tx.clone();
    let p = js_sys::Promise::new(&mut |resolve, reject| {
        let ok = Closure::once_into_js(move || {
            let _ = resolve.call0(&JsValue::NULL);
        });
        let rej = reject.clone();
        let failed = t1.clone();
        let fail = Closure::once_into_js(move |e: JsValue| {
            let _ = rej.call1(&JsValue::NULL, &failure(e, failed.as_ref()));
        });
        let aborted = t1.clone();
        let abort = Closure::once_into_js(move |e: JsValue| {
            let _ = reject.call1(&JsValue::NULL, &failure(e, aborted.as_ref()));
        });
        t1.set_oncomplete(Some(ok.unchecked_ref()));
        t1.set_onerror(Some(fail.unchecked_ref()));
        t1.set_onabort(Some(abort.unchecked_ref()));
    });
    JsFuture::from(p).await.map(|_| ()).map_err(err)
}

impl Backend {
    /// OPFS if writable from here (unless `prefer_idb`), else IndexedDB.
    pub async fn open(prefer_idb: bool) -> Result<Backend, String> {
        if !prefer_idb
            && opfs_writable()
            && let Some(sm) = storage_manager()
        {
            match JsFuture::from(sm.get_directory()).await {
                Ok(root) => return Ok(Backend::Opfs(root.unchecked_into())),
                Err(e) => log::warn!("OPFS unavailable ({}); using IndexedDB", err(e)),
            }
        }
        Self::open_idb().await
    }

    async fn open_idb() -> Result<Backend, String> {
        let f = indexed_db().ok_or("no IndexedDB")?;
        let req = f.open_with_u32(IDB_NAME, 1).map_err(err)?;
        let upgrade = Closure::<dyn FnMut(web_sys::IdbVersionChangeEvent)>::new(move |e: web_sys::IdbVersionChangeEvent| {
            if let Some(db) = e.target().and_then(|t| t.dyn_into::<web_sys::IdbOpenDbRequest>().ok()).and_then(|r| r.result().ok()) {
                let db: IdbDatabase = db.unchecked_into();
                if !db.object_store_names().contains(IDB_STORE) {
                    let _ = db.create_object_store(IDB_STORE);
                }
            }
        });
        req.set_onupgradeneeded(Some(upgrade.as_ref().unchecked_ref()));
        let db = idb_req(&req).await?;
        drop(upgrade);
        Ok(Backend::Idb(db.unchecked_into()))
    }

    /// "opfs" or "idb" (workers open the same kind).
    pub fn kind(&self) -> &'static str {
        match self {
            Backend::Opfs(_) => "opfs",
            Backend::Idb(_) => "idb",
        }
    }

    /// The directory holding `path`'s last component (created when `create`).
    async fn parent(root: &FileSystemDirectoryHandle, path: &str, create: bool) -> Result<Option<(FileSystemDirectoryHandle, String)>, String> {
        let mut parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let name = parts.pop().ok_or("empty path")?.to_string();
        let mut dir = root.clone();
        for p in parts {
            let opts = FileSystemGetDirectoryOptions::new();
            opts.set_create(create);
            match JsFuture::from(dir.get_directory_handle_with_options(p, &opts)).await {
                Ok(d) => dir = d.unchecked_into(),
                Err(e) if !create && missing(&e) => {
                    return Ok(None);
                }
                Err(e) => return Err(err(e)),
            }
        }
        Ok(Some((dir, name)))
    }

    async fn opfs_file(root: &FileSystemDirectoryHandle, path: &str, create: bool) -> Result<Option<FileSystemFileHandle>, String> {
        let Some((dir, name)) = Self::parent(root, path, create).await? else { return Ok(None) };
        let opts = FileSystemGetFileOptions::new();
        opts.set_create(create);
        match JsFuture::from(dir.get_file_handle_with_options(&name, &opts)).await {
            Ok(h) => Ok(Some(h.unchecked_into())),
            Err(e) if !create && missing(&e) => Ok(None),
            Err(e) => Err(err(e)),
        }
    }

    fn idb_store(db: &IdbDatabase, write: bool) -> Result<(web_sys::IdbTransaction, web_sys::IdbObjectStore), String> {
        let mode = if write { web_sys::IdbTransactionMode::Readwrite } else { web_sys::IdbTransactionMode::Readonly };
        let tx = db.transaction_with_str_and_mode(IDB_STORE, mode).map_err(err)?;
        let store = tx.object_store(IDB_STORE).map_err(err)?;
        Ok((tx, store))
    }

    /// A whole file, or `None` if it doesn't exist.
    pub async fn read(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        self.read_limited(path, u64::MAX).await
    }
    pub async fn read_limited(&self, path: &str, limit: u64) -> Result<Option<Vec<u8>>, String> {
        match self {
            Backend::Opfs(root) => {
                let Some(h) = Self::opfs_file(root, path, false).await? else { return Ok(None) };
                let file: web_sys::File = JsFuture::from(h.get_file()).await.map_err(err)?.unchecked_into();
                if !file.size().is_finite() || file.size() < 0. || file.size() > limit as f64 {
                    return Err("browser stored file exceeds its verified size limit; retained".into());
                }
                let buf = JsFuture::from(file.array_buffer()).await.map_err(err)?;
                Ok(Some(Uint8Array::new(&buf).to_vec()))
            }
            Backend::Idb(db) => {
                let (_tx, store) = Self::idb_store(db, false)?;
                let v = idb_req(&store.get(&path.into()).map_err(err)?).await?;
                if v.is_undefined() || v.is_null() {
                    return Ok(None);
                }
                let bytes = Uint8Array::new(&v);
                if u64::from(bytes.length()) > limit {
                    return Err("browser stored file exceeds its verified size limit; retained".into());
                }
                Ok(Some(bytes.to_vec()))
            }
        }
    }

    /// Replace a file atomically (OPFS: the writable's swap file is committed on close).
    pub async fn write(&self, path: &str, data: &[u8]) -> Result<(), String> {
        self.write_at(path, None, data).await
    }

    /// Write `data` at `offset` keeping the bytes before it (`None`: replace the file). Atomic.
    pub async fn write_at(&self, path: &str, offset: Option<usize>, data: &[u8]) -> Result<(), String> {
        match self {
            Backend::Opfs(root) => {
                let h = Self::opfs_file(root, path, true).await?.ok_or("can't create file")?;
                let opts = web_sys::FileSystemCreateWritableOptions::new();
                opts.set_keep_existing_data(offset.is_some());
                let w: web_sys::FileSystemWritableFileStream =
                    JsFuture::from(h.create_writable_with_options(&opts)).await.map_err(err)?.unchecked_into();
                let res = async {
                    if let Some(off) = offset {
                        JsFuture::from(w.truncate_with_f64(off as f64).map_err(err)?).await.map_err(err)?;
                        JsFuture::from(w.seek_with_f64(off as f64).map_err(err)?).await.map_err(err)?;
                    }
                    JsFuture::from(w.write_with_js_u8_array(&Uint8Array::from(data)).map_err(err)?).await.map_err(err)?;
                    Ok::<(), String>(())
                }
                .await;
                match res {
                    Ok(()) => JsFuture::from(w.close()).await.map(|_| ()).map_err(err),
                    Err(e) => {
                        let _ = JsFuture::from(w.abort()).await;
                        Err(e)
                    }
                }
            }
            Backend::Idb(db) => {
                let (tx, store) = Self::idb_store(db, true)?;
                let value = match offset {
                    // read-modify-write inside one transaction
                    Some(off) => {
                        let old = idb_req(&store.get(&path.into()).map_err(err)?).await?;
                        let mut v = if old.is_undefined() || old.is_null() { Vec::new() } else { Uint8Array::new(&old).to_vec() };
                        v.truncate(off);
                        v.extend_from_slice(data);
                        Uint8Array::from(&v[..])
                    }
                    None => Uint8Array::from(data),
                };
                store.put_with_key(&value, &path.into()).map_err(err)?;
                idb_done(&tx).await
            }
        }
    }

    /// Is there a file at `path`?
    pub async fn exists(&self, path: &str) -> Result<bool, String> {
        match self {
            Backend::Opfs(root) => Ok(Self::opfs_file(root, path, false).await?.is_some()),
            Backend::Idb(db) => {
                let (_tx, store) = Self::idb_store(db, false)?;
                let n = idb_req(&store.count_with_key(&path.into()).map_err(err)?).await?;
                Ok(n.as_f64().is_some_and(|n| n > 0.0))
            }
        }
    }

    pub async fn remove(&self, path: &str) -> Result<(), String> {
        match self {
            Backend::Opfs(root) => {
                let Some((dir, name)) = Self::parent(root, path, false).await? else { return Ok(()) };
                match JsFuture::from(dir.remove_entry(&name)).await {
                    Ok(_) => Ok(()),
                    Err(e) if Reflect::get(&e, &"name".into()).ok().and_then(|n| n.as_string()).as_deref() == Some("NotFoundError") => Ok(()),
                    Err(e) => Err(err(e)),
                }
            }
            Backend::Idb(db) => {
                let (tx, store) = Self::idb_store(db, true)?;
                store.delete(&path.into()).map_err(err)?;
                idb_done(&tx).await
            }
        }
    }

    /// Names of the files directly inside `dir`.
    pub async fn list(&self, dir: &str) -> Result<Vec<String>, String> {
        match self {
            Backend::Opfs(root) => {
                // `parent` of a child path is `dir` itself
                let Some((handle, _)) = Self::parent(root, &format!("{dir}/-"), false).await? else { return Ok(Vec::new()) };
                dir_keys(&handle).await
            }
            Backend::Idb(db) => {
                let (_tx, store) = Self::idb_store(db, false)?;
                let prefix = format!("{dir}/");
                let range = web_sys::IdbKeyRange::bound(&prefix.as_str().into(), &format!("{prefix}\u{ffff}").into()).map_err(err)?;
                let keys = idb_req(&store.get_all_keys_with_key(&range).map_err(err)?).await?;
                let arr: js_sys::Array = keys.unchecked_into();
                Ok(arr
                    .iter()
                    .filter_map(|k| k.as_string())
                    .filter_map(|k| k.strip_prefix(&prefix).filter(|r| !r.contains('/')).map(str::to_string))
                    .collect())
            }
        }
    }

    /// Bounded names including directory components, with matching OPFS/IDB root semantics.
    pub async fn names_bounded(&self, dir: &str, limit: usize) -> Result<Vec<String>, String> {
        match self {
            Backend::Opfs(root) => {
                let Some((handle, _)) = Self::parent(root, &format!("{dir}/-"), false).await? else { return Ok(Vec::new()) };
                dir_keys_bounded(&handle, limit).await
            }
            Backend::Idb(db) => {
                let (_tx, store) = Self::idb_store(db, false)?;
                let prefix = if dir.is_empty() { String::new() } else { format!("{dir}/") };
                let range = web_sys::IdbKeyRange::bound(&prefix.as_str().into(), &format!("{prefix}\u{ffff}").into()).map_err(err)?;
                const MAX_KEYS: u32 = 200_000;
                let keys = idb_req(&store.get_all_keys_with_key_and_limit(&range, MAX_KEYS + 1).map_err(err)?).await?;
                let keys: js_sys::Array = keys.unchecked_into();
                if keys.length() > MAX_KEYS {
                    return Err("Too many browser storage keys to check safely; saved copies retained.".into());
                }
                let mut names = std::collections::BTreeSet::new();
                for key in keys.iter() {
                    let key = key.as_string().ok_or("Unknown browser storage key format; saved copies retained.")?;
                    let tail = key.strip_prefix(&prefix).ok_or("Browser storage returned a key outside its requested folder.")?;
                    let name =
                        tail.split('/').next().filter(|name| !name.is_empty()).ok_or("Invalid browser storage path; saved copies retained.")?;
                    names.insert(name.to_owned());
                    if names.len() > limit {
                        return Err("Too many browser storage entries to check safely; saved copies retained.".into());
                    }
                }
                Ok(names.into_iter().collect())
            }
        }
    }
    /// Verify actual bytes without copying a whole cached original into Rust memory.
    pub async fn file_digest(&self, path: &str, limit: u64) -> Result<Option<(u64, String)>, String> {
        use sha2::{Digest, Sha256};
        const CHUNK: u32 = 64 << 10;
        let mut digest = Sha256::new();
        let size = match self {
            Backend::Opfs(root) => {
                let Some(handle) = Self::opfs_file(root, path, false).await? else { return Ok(None) };
                let file: web_sys::File = JsFuture::from(handle.get_file()).await.map_err(err)?.unchecked_into();
                if !file.size().is_finite() || file.size() < 0. || file.size() > limit as f64 {
                    return Err("Browser cache file exceeds its recovery size limit; copies retained.".into());
                }
                let size = file.size() as u64;
                let mut offset = 0u64;
                while offset < size {
                    let next = offset.saturating_add(u64::from(CHUNK)).min(size);
                    let part = file.slice_with_f64_and_f64(offset as f64, next as f64).map_err(err)?;
                    let bytes = Uint8Array::new(&JsFuture::from(part.array_buffer()).await.map_err(err)?);
                    if u64::from(bytes.length()) != next.saturating_sub(offset) {
                        return Err("Browser cache file changed during verification; copies retained.".into());
                    }
                    digest.update(bytes.to_vec());
                    offset = next;
                }
                size
            }
            Backend::Idb(db) => {
                let (_tx, store) = Self::idb_store(db, false)?;
                let value = idb_req(&store.get(&path.into()).map_err(err)?).await?;
                if value.is_undefined() || value.is_null() {
                    return Ok(None);
                };
                if !value.is_instance_of::<Uint8Array>() {
                    return Err("Unknown browser cache value; saved copies retained.".into());
                }
                let bytes: Uint8Array = value.unchecked_into();
                let size = bytes.length();
                if u64::from(size) > limit {
                    return Err("Browser cache file exceeds its recovery size limit; copies retained.".into());
                }
                let mut offset = 0u32;
                while offset < size {
                    let next = offset.saturating_add(CHUNK).min(size);
                    digest.update(bytes.subarray(offset, next).to_vec());
                    offset = next;
                    if offset % (4 << 20) == 0 {
                        audit_yield().await?;
                    }
                }
                u64::from(size)
            }
        };
        Ok(Some((size, format!("{:x}", digest.finalize()))))
    }

    /// Delete everything (the "reset" escape hatch: `?reset` in the URL).
    pub async fn clear(&self) -> Result<(), String> {
        match self {
            Backend::Opfs(root) => {
                for name in dir_keys(root).await? {
                    let opts = web_sys::FileSystemRemoveOptions::new();
                    opts.set_recursive(true);
                    JsFuture::from(root.remove_entry_with_options(&name, &opts)).await.map_err(err)?;
                }
                Ok(())
            }
            Backend::Idb(db) => {
                let (tx, store) = Self::idb_store(db, true)?;
                store.clear().map_err(err)?;
                idb_done(&tx).await
            }
        }
    }
}

/// Names of the entries in an OPFS directory.
async fn dir_keys(dir: &FileSystemDirectoryHandle) -> Result<Vec<String>, String> {
    dir_keys_bounded(dir, 200_000).await
}
async fn dir_keys_bounded(dir: &FileSystemDirectoryHandle, limit: usize) -> Result<Vec<String>, String> {
    let keys = dir.keys();
    let mut out = Vec::new();
    loop {
        let next = JsFuture::from(keys.next().map_err(err)?).await.map_err(err)?;
        if Reflect::get(&next, &"done".into()).ok().is_some_and(|d| d.is_truthy()) {
            break;
        }
        if let Some(k) = Reflect::get(&next, &"value".into()).ok().and_then(|v| v.as_string()) {
            if out.len() >= limit {
                return Err("Too many browser storage entries to check safely; copies retained.".into());
            }
            out.push(k);
        }
    }
    Ok(out)
}

/// Ask the browser not to evict our storage under pressure; `on_result(granted)` once known
/// (`false` also when the browser has no storage manager).
pub fn request_persistence(on_result: impl FnOnce(bool) + 'static) {
    let Some(sm) = storage_manager() else {
        on_result(false);
        return;
    };
    wasm_bindgen_futures::spawn_local(async move {
        // already persistent: nothing to ask
        let already = match sm.persisted() {
            Ok(p) => JsFuture::from(p).await.ok().is_some_and(|v| v.is_truthy()),
            Err(_) => false,
        };
        let granted = already
            || match sm.persist() {
                Ok(p) => JsFuture::from(p).await.ok().is_some_and(|v| v.is_truthy()),
                Err(_) => false,
            };
        log::info!("lightcraft: persistent storage {}", if granted { "granted" } else { "not granted" });
        on_result(granted);
    });
}

async fn audit_yield() -> Result<(), String> {
    let global = js_sys::global();
    let timer = Reflect::get(&global, &"setTimeout".into()).map_err(err)?.dyn_into::<js_sys::Function>().map_err(err)?;
    let pause = js_sys::Promise::new(&mut |resolve, reject| {
        if let Err(error) = timer.call2(&global, resolve.as_ref(), &JsValue::from_f64(0.)) {
            let _ = reject.call1(&JsValue::UNDEFINED, &error);
        }
    });
    JsFuture::from(pause).await.map(|_| ()).map_err(err)
}
