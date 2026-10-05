use std::fmt;
use std::io::Read;
use std::path::Path;

use serde::de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::index::{IndexData, OrderedMap, STAGE, SpriteInfo, StageInfo, VarInfo, VarKind};

struct FirstString(String);

impl<'de> Deserialize<'de> for FirstString {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = FirstString;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a [name, ...] array")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<FirstString, A::Error> {
                let name: String = seq.next_element()?.unwrap_or_default();
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(FirstString(name))
            }
        }
        d.deserialize_seq(V)
    }
}

#[derive(Deserialize)]
struct Named {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize, Default)]
struct Mutation {
    proccode: Option<String>,
}

enum BlockLite {
    Prototype(Option<String>),
    Other,
}

impl<'de> Deserialize<'de> for BlockLite {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = BlockLite;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a block")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<BlockLite, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(BlockLite::Other)
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<BlockLite, A::Error> {
                let mut prototype = false;
                let mut proccode = None;
                while let Some(key) = map.next_key::<std::borrow::Cow<'de, str>>()? {
                    match &*key {
                        "opcode" => prototype = map.next_value::<std::borrow::Cow<'de, str>>()? == "procedures_prototype",
                        "mutation" => proccode = map.next_value::<Option<Mutation>>()?.and_then(|m| m.proccode),
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(if prototype { BlockLite::Prototype(proccode) } else { BlockLite::Other })
            }
            fn visit_unit<E: de::Error>(self) -> Result<BlockLite, E> {
                Ok(BlockLite::Other)
            }
        }
        d.deserialize_any(V)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Target {
    #[serde(default)]
    is_stage: bool,
    #[serde(default)]
    name: String,
    #[serde(default)]
    variables: OrderedMap<FirstString>,
    #[serde(default)]
    lists: OrderedMap<FirstString>,
    #[serde(default)]
    broadcasts: OrderedMap<String>,
    #[serde(default)]
    blocks: OrderedMap<BlockLite>,
    #[serde(default)]
    costumes: Vec<Named>,
    #[serde(default)]
    sounds: Vec<Named>,
}

#[derive(Deserialize, Default)]
struct MonitorParams {
    #[serde(rename = "VARIABLE")]
    variable: Option<String>,
    #[serde(rename = "LIST")]
    list: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Monitor {
    #[serde(default)]
    opcode: String,
    #[serde(default)]
    params: MonitorParams,
    #[serde(default)]
    sprite_name: Option<String>,
    #[serde(default)]
    visible: bool,
}

#[derive(Deserialize)]
struct ProjectJson {
    #[serde(default)]
    targets: Vec<Target>,
    #[serde(default)]
    monitors: Vec<Monitor>,
}

pub fn index_from_project_json(bytes: &[u8]) -> Result<IndexData, String> {
    let project: ProjectJson = serde_json::from_slice(bytes).map_err(|e| format!("project.json: {e}"))?;
    let mut index = IndexData::default();
    for target in project.targets {
        let costumes: Vec<String> = target.costumes.into_iter().map(|c| c.name).collect();
        let sounds: Vec<String> = target.sounds.into_iter().map(|s| s.name).collect();
        let vars: Vec<VarInfo> = target
            .variables
            .into_iter()
            .map(|(id, v)| VarInfo { name: v.0, id, kind: VarKind::Variable })
            .chain(target.lists.into_iter().map(|(id, v)| VarInfo { name: v.0, id, kind: VarKind::List }))
            .collect();
        if target.is_stage {
            index.stage = StageInfo { backdrops: costumes, sounds };
            index.global_variables = vars;
            index.broadcasts = target.broadcasts.into_values().collect();
        } else {
            let custom: Vec<String> = target
                .blocks
                .into_values()
                .filter_map(|b| match b {
                    BlockLite::Prototype(code) => code.filter(|c| !c.is_empty()),
                    BlockLite::Other => None,
                })
                .collect();
            index.sprites.push(SpriteInfo { name: target.name.clone(), costumes, sounds });
            index.sprite_variables.insert(target.name.clone(), vars);
            index.custom_blocks.insert(target.name, custom);
        }
    }
    index.monitored = project
        .monitors
        .into_iter()
        .filter(|m| m.visible)
        .filter_map(|m| {
            let name = m.params.variable.or(m.params.list).filter(|n| !n.is_empty())?;
            let kind = if m.opcode == "data_listcontents" { VarKind::List } else { VarKind::Variable };
            Some((m.sprite_name.filter(|s| !s.is_empty()).unwrap_or_else(|| STAGE.to_string()), kind, name))
        })
        .collect();
    Ok(index)
}

pub fn zip_entry(bytes: &[u8], wanted: &str) -> Result<Vec<u8>, String> {
    let u16_at = |i: usize| bytes.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize).ok_or("truncated zip");
    let u32_at = |i: usize| bytes.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize).ok_or("truncated zip");
    let mut eocd = bytes.len().checked_sub(22).ok_or("not a zip file")?;
    while u32_at(eocd)? != 0x0605_4b50 {
        eocd = eocd.checked_sub(1).ok_or("not a zip file")?;
    }
    let count = u16_at(eocd + 10)?;
    let mut offset = u32_at(eocd + 16)?;
    for _ in 0..count {
        let method = u16_at(offset + 10)?;
        let size = u32_at(offset + 20)?;
        let name_len = u16_at(offset + 28)?;
        let extra_len = u16_at(offset + 30)?;
        let comment_len = u16_at(offset + 32)?;
        let local = u32_at(offset + 42)?;
        let name = bytes.get(offset + 46..offset + 46 + name_len).ok_or("truncated zip")?;
        if name == wanted.as_bytes() {
            let start = local + 30 + u16_at(local + 26)? + u16_at(local + 28)?;
            let data = bytes.get(start..start + size).ok_or("truncated zip entry")?;
            return match method {
                0 => Ok(data.to_vec()),
                8 => miniz_oxide::inflate::decompress_to_vec(data).map_err(|e| format!("inflate failed: {e:?}")),
                other => Err(format!("unsupported zip compression method {other}")),
            };
        }
        offset += 46 + name_len + extra_len + comment_len;
    }
    Err(format!("{wanted} not found in archive"))
}

pub fn read_index_source(path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path).and_then(|mut f| f.read_to_end(&mut bytes)).map_err(|e| format!("{}: {e}", path.display()))?;
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("sb3")) {
        return zip_entry(&bytes, "project.json");
    }
    Ok(bytes)
}

pub fn load_index(path: &Path) -> Result<IndexData, String> {
    index_from_project_json(&read_index_source(path)?)
}
