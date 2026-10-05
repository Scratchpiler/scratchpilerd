use std::sync::Arc;

use indexmap::IndexMap;
use rustc_hash::{FxBuildHasher, FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

pub type OrderedMap<V> = IndexMap<String, V, FxBuildHasher>;

pub const STAGE: &str = "__stage__";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VarKind {
    Variable,
    List,
}

impl VarKind {
    pub fn js(self) -> &'static str {
        match self {
            VarKind::Variable => "variable",
            VarKind::List => "list",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VarInfo {
    pub name: String,
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type")]
    pub kind: VarKind,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SpriteInfo {
    pub name: String,
    #[serde(default)]
    pub costumes: Vec<String>,
    #[serde(default)]
    pub sounds: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StageInfo {
    #[serde(default)]
    pub backdrops: Vec<String>,
    #[serde(default)]
    pub sounds: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexData {
    #[serde(default)]
    pub sprites: Vec<SpriteInfo>,
    #[serde(default)]
    pub stage: StageInfo,
    #[serde(default)]
    pub global_variables: Vec<VarInfo>,
    #[serde(default)]
    pub sprite_variables: OrderedMap<Vec<VarInfo>>,
    #[serde(default)]
    pub custom_blocks: OrderedMap<Vec<String>>,
    #[serde(default)]
    pub broadcasts: Vec<String>,
    #[serde(default)]
    pub monitored: Vec<(String, VarKind, String)>,
}

#[derive(Default, Debug)]
pub struct VarSets {
    pub vars: FxHashSet<Box<str>>,
    pub lists: FxHashSet<Box<str>>,
}

#[derive(Debug)]
pub struct ScratchIndex {
    pub data: IndexData,
    pub hash: u64,
    pub from_project: bool,
    sets: FxHashMap<String, VarSets>,
    globals: VarSets,
}

impl ScratchIndex {
    pub fn new(data: IndexData, from_project: bool) -> Arc<ScratchIndex> {
        let encoded = bincode::serialize(&(&data, from_project)).unwrap_or_default();
        let hash = xxhash_rust::xxh3::xxh3_64(&encoded);
        let collect = |vars: &[&VarInfo]| {
            let mut sets = VarSets::default();
            for v in vars {
                match v.kind {
                    VarKind::List => sets.lists.insert(v.name.as_str().into()),
                    VarKind::Variable => sets.vars.insert(v.name.as_str().into()),
                };
            }
            sets
        };
        let globals: Vec<&VarInfo> = data.global_variables.iter().collect();
        let mut sets = FxHashMap::default();
        for (sprite, vars) in &data.sprite_variables {
            let all: Vec<&VarInfo> = data.global_variables.iter().chain(vars.iter()).collect();
            sets.insert(sprite.clone(), collect(&all));
        }
        Arc::new(ScratchIndex { globals: collect(&globals), sets, data, hash, from_project })
    }

    pub fn empty() -> Arc<ScratchIndex> {
        ScratchIndex::new(IndexData::default(), false)
    }

    pub fn var_sets(&self, sprite: &str) -> &VarSets {
        self.sets.get(sprite).unwrap_or(&self.globals)
    }

    pub fn sprite_vars(&self, sprite: &str) -> &[VarInfo] {
        self.data.sprite_variables.get(sprite).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn sprite_names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(STAGE).chain(self.data.sprites.iter().map(|s| s.name.as_str()))
    }

    pub fn sprite(&self, name: &str) -> Option<&SpriteInfo> {
        self.data.sprites.iter().find(|s| s.name == name)
    }
}
