//! Derived, nonserialized reverse identities. Rebuilt once after loading a snapshot.
use std::{collections::BTreeMap, sync::OnceLock};

#[derive(Clone, Debug, Default)]
pub(crate) struct Index(OnceLock<BTreeMap<String, String>>);
impl PartialEq for Index {
    fn eq(&self, _: &Self) -> bool {
        true // A cache is not catalog data.
    }
}
impl Index {
    pub(crate) fn get<'a>(&'a self, identities: &BTreeMap<String, String>) -> &'a BTreeMap<String, String> {
        self.0.get_or_init(|| identities.iter().map(|(local, global)| (global.clone(), local.clone())).collect())
    }
    pub(crate) fn update(&mut self, local: &str, old: Option<&str>, new: Option<&str>) {
        if let Some(index) = self.0.get_mut() {
            if let Some(old) = old {
                index.remove(old);
            }
            if let Some(new) = new {
                index.insert(new.into(), local.into());
            }
        }
    }
}
