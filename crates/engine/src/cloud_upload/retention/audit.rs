use super::*;
use lightcraft_catalog::replication::{AppendProgress, Portable, archive};

#[derive(Default)]
pub(super) struct Audit {
    pub(super) b2: bool,
    strings: BTreeSet<String>,
    clocks: BTreeMap<(String, String), u64>,
    mutations: BTreeMap<(String, String, u64), String>,
    receipts: BTreeMap<(String, String, u64), String>,
    nodes: archive::Witness,
    scopes: BTreeMap<String, (String, String, u64, u64)>,
    required: BTreeSet<String>,
    sizes: BTreeMap<String, u64>,
    scanned: BTreeMap<String, u64>,
}
impl Audit {
    pub fn references(&mut self, value: &Value) -> Result<()> {
        let mut todo = vec![(value, 0usize)];
        let mut count = 0usize;
        while let Some((value, depth)) = todo.pop() {
            count = count.saturating_add(1);
            if count > 1_000_000 || depth > 100 {
                return Err(error("retention reference graph exceeds its safety limit; nothing deleted"));
            }
            match value {
                Value::String(s) if digest(s) || s.len() <= 2048 && s.contains('/') => {
                    self.strings.insert(s.clone());
                }
                Value::Array(values) => todo.extend(values.iter().map(|v| (v, depth + 1))),
                Value::Object(values) => {
                    for key in values.keys().filter(|key| digest(key)) {
                        self.strings.insert(key.clone());
                    }
                    if let (Some(key), Some(size), Some(_)) =
                        (values.get("key").and_then(Value::as_str), values.get("size").and_then(Value::as_u64), values.get("hash"))
                        && self.sizes.insert(key.into(), size).is_some_and(|old| old != size)
                    {
                        return Err(error("metadata reference length collision; nothing deleted"));
                    }
                    todo.extend(values.values().map(|v| (v, depth + 1)));
                }
                _ => {}
            }
            if self.strings.len() > 100_000 {
                return Err(error("too many retention references; nothing deleted"));
            }
        }
        Ok(())
    }
    fn clock(&mut self, library: &str, clock: &Clock) {
        for (author, sequence) in clock {
            let target = self.clocks.entry((library.into(), author.clone())).or_default();
            *target = (*target).max(*sequence);
        }
    }
    pub fn local_clocks(&mut self, prefix: &str, local: &Value) -> Result<()> {
        let state: lightcraft_catalog::replication::SyncState = serde_json::from_value(local["state"].clone()).map_err(|e| error(e.to_string()))?;
        state.validate().map_err(|e| error(e.to_string()))?;
        self.portable(prefix, &state.portable())
    }
    fn scope(&mut self, prefix: &str, hash: &str, library: &str, author: &str, first: u64, count: u64) -> Result<()> {
        self.required.insert(crate::cloud_history::key(prefix, library, hash).map_err(|e| error(e.to_string()))?);
        let scope = (library.into(), author.into(), first, count);
        if self.scopes.insert(hash.into(), scope.clone()).is_some_and(|old| old != scope) {
            return Err(error("receipt archive scope collision; retention refused"));
        }
        Ok(())
    }
    fn receipt(
        &mut self,
        library: &str,
        stamp: &lightcraft_catalog::replication::Stamp,
        receipt: &lightcraft_catalog::replication::Receipt,
    ) -> Result<String> {
        let leaf = archive::Node {
            version: 1,
            library: library.into(),
            author: stamp.device.clone(),
            first: stamp.sequence,
            count: 1,
            value: archive::Value::Leaf {
                digest: receipt.digest.clone(),
                context: sha256(&serde_json::to_vec(&receipt.context).map_err(|e| error(e.to_string()))?).0,
            },
        };
        let hash = leaf.digest().map_err(|e| error(e.to_string()))?;
        let key = (library.into(), stamp.device.clone(), stamp.sequence);
        if self.receipts.insert(key, hash.clone()).is_some_and(|old| old != hash) || self.receipts.len() > 100_000 {
            return Err(error("receipt identity/context collision or oversized history; retention refused"));
        }
        Ok(hash)
    }
    fn portable(&mut self, prefix: &str, state: &Portable) -> Result<()> {
        state.validate().map_err(|e| error(e.to_string()))?;
        self.clock(&state.library, &state.clock);
        for (id, receipt) in &state.received {
            // Portable::validate above has checked the canonical author-sequence ID.
            let (author, sequence) = id.rsplit_once('-').ok_or_else(|| error("invalid receipt identity"))?;
            let stamp = lightcraft_catalog::replication::Stamp {
                device: author.into(),
                sequence: sequence.parse().map_err(|_| error("invalid receipt sequence"))?,
            };
            self.receipt(&state.library, &stamp, receipt)?;
        }
        for (author, archive) in &state.archives {
            let mut first = 1u64;
            let mut roots = archive.roots.iter();
            for bit in (0..64).rev() {
                let count = 1u64 << bit;
                if archive.through & count != 0 {
                    let root = roots.next().ok_or_else(|| error("missing receipt archive root"))?;
                    self.scope(prefix, root, &state.library, author, first, count)?;
                    first = first.saturating_add(count);
                }
            }
        }
        Ok(())
    }
    fn mutation(&mut self, mutation: &Mutation) -> Result<()> {
        mutation.validate().map_err(|e| error(e.to_string()))?;
        if mutation.version != 2 {
            return Err(error("obsolete mutation format; retention refused"));
        }
        self.clock(&mutation.library, &mutation.context);
        self.clock(&mutation.library, &[(mutation.revision.device.clone(), mutation.revision.sequence)].into());
        let key = (mutation.library.clone(), mutation.revision.device.clone(), mutation.revision.sequence);
        let receipt = crate::cloud_history::receipt(mutation).map_err(|e| error(e.to_string()))?;
        let digest = self.receipt(&mutation.library, &mutation.revision, &receipt)?;
        if self.mutations.insert(key, digest.clone()).is_some_and(|old| old != digest) || self.mutations.len() > 100_000 {
            return Err(error("mutation identity collision or oversized history; retention refused"));
        }
        Ok(())
    }
    pub fn record(&mut self, prefix: &str, key: &str, bytes: &[u8]) -> Result<()> {
        let bad = || error(format!("unknown, corrupt or obsolete retention metadata at {key}; nothing deleted"));
        let relative = key.strip_prefix(prefix).and_then(|tail| tail.strip_prefix('/')).ok_or_else(bad)?;
        let value: Value = serde_json::from_slice(bytes).map_err(|_| bad())?;
        self.references(&value)?;
        let parts: Vec<_> = relative.split('/').collect();
        // Change batches and checkpoints live in epoch 1 (`catalog/`) or a rebased epoch namespace.
        let digest = sha256(bytes).0;
        let record = |namespace| crate::cloud_records::object_epoch(prefix, namespace, key, &digest).is_some();
        match parts.as_slice() {
            ["catalog", "head.json"] => {
                let head = crate::cloud_records::Head::decode(bytes).map_err(|_| bad())?;
                head.validate_scope(prefix, head.library.as_deref().ok_or_else(bad)?).map_err(|_| bad())?;
                if head.version < 3 {
                    return Err(bad());
                }
            }
            ["catalog", "b2-authority.json"] if self.b2 => {
                if value.get("version").and_then(Value::as_u64) != Some(1)
                    || value.get("protocol").and_then(Value::as_str) != Some("b2-bucket-head-v1")
                    || !value.get("binding").and_then(Value::as_str).is_some_and(super::digest)
                    || value.as_object().is_none_or(|fields| fields.len() != 3)
                {
                    return Err(bad());
                }
            }
            ["catalog", "authority", name] if self.b2 && *name == format!("{digest}.json") => {
                let head = crate::cloud_records::Head::decode(bytes).map_err(|_| bad())?;
                head.validate_scope(prefix, head.library.as_deref().ok_or_else(bad)?).map_err(|_| bad())?;
            }
            _ if self.b2 && crate::cloud_records::object_epoch(prefix, "legacy", key, &digest) == Some(1) => {
                let library = value.get("library").and_then(Value::as_str).ok_or_else(bad)?;
                crate::cloud_records::LegacyIntake::decode(bytes, library).map_err(|_| bad())?;
                self.required.insert(crate::cloud_append::manifest_key(prefix, library).map_err(|_| bad())?);
            }
            _ if record("changes") => {
                let batch: crate::cloud_records::Batch = serde_json::from_slice(bytes).map_err(|_| bad())?;
                batch.validate_scope(prefix, batch.mutations.first().map_or("", |m| m.library.as_str())).map_err(|_| bad())?;
                if batch.version != 3 || !batch.imports.is_empty() {
                    return Err(bad());
                }
                for mutation in &batch.mutations {
                    self.mutation(mutation)?;
                }
            }
            _ if record("checkpoints") => {
                let checkpoint: crate::cloud_records::Checkpoint = serde_json::from_slice(bytes).map_err(|_| bad())?;
                if checkpoint.version != 1 || checkpoint.generation == 0 || !checkpoint.imports.is_empty() {
                    return Err(bad());
                }
                checkpoint.covers.validate_scope(prefix, "changes", 2 << 20).map_err(|_| bad())?;
                self.portable(prefix, &checkpoint.state)?;
            }
            ["sync-v2", library, "control", _] => {
                crate::cloud_append::control::effective(prefix, library, &[(key.to_owned(), bytes.to_vec())]).map_err(|_| bad())?;
            }
            ["sync-v2", library, "manifest", _] => {
                crate::cloud_append::immutable_metadata(key, bytes).map_err(|_| bad())?;
                if bytes != crate::cloud_append::manifest(library).map_err(|_| bad())? {
                    return Err(bad());
                }
            }
            ["sync-v2", library, "ops", _, _] => {
                let mutation = crate::cloud_append::decode_mutation(prefix, library, key, bytes).map_err(|_| bad())?;
                self.mutation(&mutation)?;
            }
            ["sync-v2", library, "checkpoints", _, _] => {
                crate::cloud_append::immutable_metadata(key, bytes).map_err(|_| bad())?;
                let progress: AppendProgress = serde_json::from_slice(bytes).map_err(|_| bad())?;
                progress.validate(library).map_err(|_| bad())?;
                if !progress.gaps.is_empty() || progress.manifest != sha256(&crate::cloud_append::manifest(library).map_err(|_| bad())?).0 {
                    return Err(bad());
                }
                self.portable(prefix, &progress.remote)?;
            }
            ["sync-v2", library, "receipts", name] => {
                let hash = name.strip_suffix(".json").ok_or_else(bad)?;
                let node = archive::Node::decode(hash, bytes).map_err(|_| bad())?;
                if node.library != *library {
                    return Err(bad());
                }
                self.clock(library, &[(node.author.clone(), node.first.saturating_add(node.count).saturating_sub(1))].into());
                if let archive::Value::Branch { left, right } = &node.value {
                    for (child, first) in [(left, node.first), (right, node.first.saturating_add(node.count / 2))] {
                        self.scope(prefix, child, library, &node.author, first, node.count / 2)?;
                    }
                }
                self.nodes.insert(hash.into(), node);
            }
            _ => return Err(bad()),
        }
        self.scanned.insert(key.into(), bytes.len() as u64);
        Ok(())
    }
    pub fn complete(&self, keys: &BTreeSet<String>) -> Result<()> {
        if self.required.iter().any(|key| !keys.contains(key))
            || self.sizes.iter().any(|(key, size)| !keys.contains(key) || self.scanned.get(key) != Some(size))
        {
            return Err(error("retained metadata reference is missing or has the wrong length; no reclamation applied"));
        }
        for (hash, (library, author, first, count)) in &self.scopes {
            if !self
                .nodes
                .get(hash)
                .is_some_and(|node| node.library == *library && node.author == *author && node.first == *first && node.count == *count)
            {
                return Err(error("receipt archive branch/root scope mismatch; no reclamation applied"));
            }
        }
        for (hash, node) in &self.nodes {
            if matches!(node.value, archive::Value::Leaf { .. })
                && self.mutations.get(&(node.library.clone(), node.author.clone(), node.first)) != Some(hash)
            {
                return Err(error("archived mutation identity/context collision; no reclamation applied"));
            }
        }
        if self.receipts.iter().any(|(key, hash)| self.mutations.get(key) != Some(hash)) {
            return Err(error("checkpoint receipt has no matching retained operation; no reclamation applied"));
        }
        let mut total = 0u64;
        for ((library, author), sequence) in &self.clocks {
            total = total.saturating_add(*sequence);
            if total > 100_000 {
                return Err(error("retention history exceeds 100,000 mutations; no reclamation applied"));
            }
            for sequence in 1..=*sequence {
                if !self.mutations.contains_key(&(library.clone(), author.clone(), sequence)) {
                    return Err(error("retained operation history is incomplete; restore missing operations before reclamation"));
                }
            }
        }
        Ok(())
    }
    pub fn protects(&self, key: &str, hash: &str) -> bool {
        self.strings.contains(key) || self.strings.contains(hash)
    }
}
