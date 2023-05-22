#![allow(irrefutable_let_patterns)]
#![feature(let_chains)]
use argh::FromArgs;
use eyre::{bail, ContextCompat, Result};
use indicatif::ParallelProgressIterator;
use parking_lot::Mutex;
use rayon::prelude::*;
use roead::{byml::Byml, sarc::Sarc};
use std::{
    fs,
    path::{Path, PathBuf},
    println,
};
use zstd::bulk::Decompressor;

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
        let mut decompressor = if name.ends_with(".bcett.byml.zs") {
            self.map_decomp.lock()
        } else if name.ends_with(".pack.zs") {
            self.pack_decomp.lock()
        } else if name.ends_with(".rsizetable.zs") {
            self.default_decomp.lock()
        } else {
            self.common_decomp.lock()
        };
        let mut last_error = None;
        for i in 2..(COMPRESSION_LEVEL * 2) {
            match decompressor.decompress(data, data.len() * i) {
                Ok(data) => return Ok(data),
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            }
        }
        eyre::bail!("Failed to decompress. {last_error:?}")
    }

    fn write_byml(&self, mut data: Vec<u8>, relative: &Path) -> Result<()> {
        let name = relative.file_name().map(|n| n.to_string_lossy()).unwrap();
        if name.ends_with(".zs") {
            data = self.decompress(&name, &data)?;
        }
        match &data[..2] {
            b"BY" => data[3] = 4,
            b"YB" => data[2] = 2,
            _ => return Ok(()),
        };
        match Byml::from_binary(&data) {
            Ok(byml) => {
                let out = self.output.join(relative).with_extension("yml");
                out.parent().map(fs::create_dir_all).transpose()?;
                match serde_yaml::to_string(&byml) {
                    Ok(text) => fs::write(out, text)?,
                    Err(_) => println!(
                        "WARNING: Could not dump {} to YAML.",
                        relative.display(),
                        // byml
                    ),
                }
            }
            Err(e) => {
                println!(
                    "WARNING: Failed to parse {}. Reason: {}",
                    relative.display(),
                    e
                );
                let mut out = self.output.join(relative);
                if name.ends_with(".zs") {
                    out.set_extension("");
                }
                out.parent().map(fs::create_dir_all).transpose()?;
                fs::write(out, data)?;
            }
        }
        Ok(())
    }

    fn unpack(&self) -> Result<()> {
        let files = jwalk::WalkDir::new(&self.source)
            .into_iter()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect::<Vec<_>>();
        let len = files.len();
        files
            .into_par_iter()
            .progress_count(len as u64)
            .try_for_each(|file| -> Result<()> {
                let name = file
                    .file_name()
                    .context("No filename")?
                    .to_str()
                    .context("Bad filename")?;
                let relative = file.strip_prefix(&self.source).unwrap();
                if name.ends_with(".byml.zs") || name.ends_with(".bgyml") {
                    let data = fs::read(&file)?;
                    self.write_byml(data, relative)?;
                } else if name.ends_with(".pack.zs") || name.ends_with(".sarc.zs") {
                    let data = self.decompress(name, &fs::read(&file)?)?;
                    let sarc = Sarc::new(data)?;
                    for file in sarc.files().filter(|f| f.name().is_some()) {
                        let name = file.unwrap_name();
                        if name.ends_with(".byml.zs") || name.ends_with(".bgyml") {
                            let data = file.data().to_vec();
                            self.write_byml(data, &relative.join(name))?;
                        } else if file.is_aamp() {
                            let pio = roead::aamp::ParameterIO::from_binary(file.data)?;
                            let out = self.output.join(relative).join(name).with_extension("yml");
                            out.parent().map(fs::create_dir_all).transpose()?;
                            fs::write(out, serde_yaml::to_string(&pio)?)?;
                        } else if file.data.starts_with(b"MsgStdBn") {
                            match msyt::Msyt::from_msbt_bytes(file.data)
                                .map_err(|e| e.chain().rev().fold(eyre::eyre!("Failed to parse MSBT"), |acc, e| acc.wrap_err(eyre::eyre!("{e}"))))
                            {
                                Ok(msbt) => {
                                    let out =
                                        self.output.join(relative).join(name).with_extension("yml");
                                    out.parent().map(fs::create_dir_all).transpose()?;
                                    match serde_yaml::to_string(&msbt) {
                                        Ok(text) => fs::write(out, text)?,
                                        Err(e) => {
                                            println!("WARNING: Failed to dump MSBT file to YAML. Error: {e:?}.")
                                        }
                                    };
                                }
                                Err(e) => println!(
                                    "WARNING: Failed to parse MSBT file {name}. Error: {e:?}."
                                ),
                            }
                        } else {
                            let out = self.output.join(relative).join(name);
                            out.parent().map(fs::create_dir_all).transpose()?;
                            fs::write(out, file.data())?;
                        }
                    }
                }
                Ok(())
            })?;
        println!("Done");
        Ok(())
    }
}

fn main() -> Result<()> {
    let args: UnpackArgs = argh::from_env();
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
    println!("Unpacking ROM to {}…", output.display());
    Unpacker::new(source, output).init_dicts()?.unpack()?;
    Ok(())
}
