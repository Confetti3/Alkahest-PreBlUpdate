use std::{
    fs::File,
    io::{BufRead, BufReader, Cursor, Read, Seek, SeekFrom},
};

use ahash::HashMap;
use alkahest_data::{
    cui::{SCuiScreen, S80803C6F},
    hash::fnv1,
    pattern::SComponent,
    tfx::sequencer::{
        SSequenceNodeBase, SUnk8080816f, SUnk80808179, SUnk808091f1, SUnk808091f1Variant,
    },
};
use chroma_dbg::{ChromaConfig, ChromaDebug};
use tiger_parse::{PackageManagerExt, TigerReadable};
use tiger_pkg::package_manager;

type Wordlist = HashMap<u32, String>;

fn main() -> anyhow::Result<()> {
    alkahest_core::initialize_package_manager(None)?;

    println!("Loading wordlist");

    let mut wordlist = HashMap::default();
    let wordlist_file =
        BufReader::new(File::open("wordlist.txt").expect("Failed to open wordlist file"));

    for line in wordlist_file.lines() {
        let line = line.expect("Failed to read line");
        wordlist.insert(fnv1(&line), line);
    }

    println!("Wordlist loaded");

    let screen: SCuiScreen = package_manager().read_tag_struct(0x80A9FBBD)?;

    println!("{}", screen.dbg_chroma());
    let thing: S80803C6F = package_manager().read_tag_struct(screen.unk1c)?;
    println!("{}", thing.dbg_chroma());
    for subthing_meta in &thing.unk8 {
        let subthing: S80803C6F = package_manager().read_tag_struct(subthing_meta.sub_component)?;
        println!("{}", subthing.dbg_chroma());
    }

    Ok(())
}

fn get_string_fnv(wordlist: &Wordlist, hash: u32) -> String {
    wordlist
        .get(&hash)
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("0x{hash:08X}"))
}
