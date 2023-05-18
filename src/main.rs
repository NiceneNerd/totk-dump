#![allow(irrefutable_let_patterns)]
#![feature(let_chains)]
use argh::FromArgs;
use eyre::{bail, Context, ContextCompat, Result};
use parking_lot::Mutex;
use rayon::prelude::*;
use roead::{byml::Byml, sarc::Sarc};
use std::{
    fs,
    path::{Path, PathBuf},
    println,
};
use zstd::{bulk::Decompressor, dict::DecoderDictionary, Decoder};

const COMPRESSION_LEVEL: usize = 15;

#[derive(FromArgs, PartialEq, Debug)]
/// Tool to unpack TOTK ROM to a human-readable, pseudosource format
struct UnpackArgs {
    /// the source folder for the TOTK ROM
    #[argh(positional)]
    source: PathBuf,
    /// the destination for the unpacked data (defaults to `./unpacked`)
    #[argh(positional)]
    output: Option<PathBuf>,
}

struct Unpacker {
    source: PathBuf,
    output: PathBuf,
    default_decomp: Mutex<Decompressor<'static>>,
    common_decomp: Mutex<Decompressor<'static>>,
    pack_decomp: Mutex<Decompressor<'static>>,
    map_decomp: Mutex<Decompressor<'static>>,
}

impl Unpacker {
    fn new(source: PathBuf, output: PathBuf) -> Self {
        Self {
            source,
            output,
            common_decomp: Default::default(),
            default_decomp: Default::default(),
            map_decomp: Default::default(),
            pack_decomp: Default::default(),
        }
    }

    fn init_dicts(self) -> Result<Self> {
        let data = fs::read(self.source.join("Pack/ZsDic.pack.zs"))?;
        let sarc = Sarc::new(
            self.default_decomp
                .lock()
                .decompress(&data, data.len() * 15)?,
        )?;
        let zs = sarc
            .get_data("zs.zsdic")
            .context("ZsDic pack missing general dictionary")?;
        self.common_decomp.lock().set_dictionary(zs)?;
        let pack = sarc
            .get_data("pack.zsdic")
            .context("ZsDic pack missing pack dictionary")?;
        self.pack_decomp.lock().set_dictionary(pack)?;
        let map = sarc
            .get_data("bcett.byml.zsdic")
            .context("ZsDic pack missing map dictionary")?;
        self.map_decomp.lock().set_dictionary(map)?;
        Ok(self)
    }

    fn decompress(&self, name: &str, data: &[u8]) -> Result<Vec<u8>> {
        if name.ends_with(".bcett.byml.zs") {
            Ok(self
                .map_decomp
                .lock()
                .decompress(data, data.len() * COMPRESSION_LEVEL)?)
        } else if name.ends_with(".pack.zs") {
            Ok(self
                .pack_decomp
                .lock()
                .decompress(data, data.len() * COMPRESSION_LEVEL)?)
        } else if name.ends_with(".rsizetable.zs") {
            Ok(self
                .default_decomp
                .lock()
                .decompress(data, data.len() * COMPRESSION_LEVEL)?)
        } else {
            Ok(self
                .common_decomp
                .lock()
                .decompress(data, data.len() * COMPRESSION_LEVEL)?)
        }
    }

    fn unpack(&self) -> Result<()> {
        jwalk::WalkDir::new(&self.source)
            .into_iter()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .par_bridge()
            .try_for_each(|file| -> Result<()> {
                let name = file
                    .file_name()
                    .context("No filename")?
                    .to_str()
                    .context("Bad filename")?;
                let relative = file.strip_prefix(&self.source).unwrap();
                if name.ends_with(".byml.zs") || name.ends_with(".bgyml") {
                    let mut data = fs::read(&file)?;
                    if name.ends_with(".zs") {
                        data = self.decompress(name, &data)?;
                    }
                    match Byml::from_binary(data) {
                        Ok(byml) => {
                            let out = self.output.join(relative).with_extension("yml");
                            out.parent().map(fs::create_dir_all).transpose()?;
                            fs::write(out, byml.to_text().unwrap())?;
                        }
                        Err(e) => {
                            println!(
                                "WARNING: Failed to parse {}. Reason: {}",
                                relative.display(),
                                e
                            );
                        }
                    }
                } else if name.ends_with(".pack.zs") {
                    println!("TODO");
                }
                Ok(())
            })?;
        println!("Done");
        Ok(())
    }
}

fn main() -> Result<()> {
    let args: UnpackArgs = argh::from_env();
    dbg!(&args);
    let mut source = args.source.canonicalize()?;
    if !source.exists() {
        bail!("Source directory does not exist");
    }
    if !source.ends_with("romfs") {
        if let subsource = source.join("romfs") && subsource.exists() {
            source = subsource
        } else if let upsource = source
            .parent()
            .context("No source folder parent")?
            .join("romfs")
            && upsource.exists()
        {
            source = upsource
        } else if let upsource = source
            .parent()
            .context("No source folder parnet")?
            .with_file_name("romfs")
            && upsource.exists()
        {
            source = upsource
        } else {
            bail!("No romfs folder found");
        }
    }
    let output = args
        .output
        .unwrap_or_else(|| std::env::current_dir().unwrap().join("unpacked"));
    Unpacker::new(source, output).init_dicts()?.unpack()?;
    Ok(())
}
