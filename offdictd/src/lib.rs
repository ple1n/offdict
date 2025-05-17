#![allow(unused_variables)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![feature(async_closure)]
#![feature(anonymous_lifetime_in_impl_trait)]
#![feature(associated_type_defaults)]
#![feature(let_chains)]

use anyhow::bail;
pub use anyhow::Result;

#[cfg(feature="fst")]
use fst_index::fstmmap;

use owo_colors::OwoColorize;
use rand::seq::SliceRandom;
use rand::thread_rng;
pub use serde::{Deserialize, Serialize};

pub use serde_yaml::{self};

pub use tokio::io::{AsyncReadExt, AsyncWriteExt};
use topk::Strprox;

use std::borrow::Borrow;
use std::collections::{self, BTreeMap, BTreeSet, HashMap};

use std::fs::remove_dir_all;
use std::io::BufWriter;
use std::iter::FromIterator;

use std::marker::PhantomData;
use std::str::FromStr;
use std::time::Duration;
use std::{self, fs, process, vec};

// use bson::{self, Array, Serializer};
pub use std::cmp::min;

pub use derive_new::new;
pub use std::error::Error;

pub use std::fs::{create_dir_all, File};
pub use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use timed::timed;

use debug_print::debug_println;
use std::cmp::Ordering::Equal;

use def_bin::DBKey;
use memmap2::Mmap;
use rocksdb::{BlockBasedOptions, Options, ReadOptions, SliceTransform, DB as rocks};
use serde_ignored;
pub mod topk;

pub type DefItemWrapped = def_bin::WrapperDef;
pub type DefItem = def_bin::Def;
pub type DB = rocks;

pub struct stat {
    pub words: usize,
    pub unique_words: Option<usize>,
}

impl std::fmt::Display for stat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("Entries in database, {}. ", self.words))?;
        if let Some(uw) = &self.unique_words {
            f.write_fmt(format_args!("Unique words in index, {}.", uw))
        } else {
            Ok(())
        }
    }
}

pub type candidate = String;
pub type candidates = Vec<candidate>;
pub struct offdict<index: Indexer> {
    db: Arc<RwLock<rocks>>,
    pub set: Option<index>,
    dirpath: PathBuf,
    /// Request the desktop client to query a word and display it.
    pub set_input: Option<fn(String, bool) -> Result<()>>,
}

pub trait Indexer: Sized + 'static {
    const FILE_NAME: &'static str;
    type Param = ();
    fn load_file(pp: &Path) -> Result<Self>;
    fn query(&self, query: &str, para: Self::Param) -> Result<candidates>;
    fn build_all(words: impl IntoIterator<Item = String>, pp: &Path) -> Result<()>;
    fn count(&self) -> usize;
    fn path(data_dir: &Path) -> PathBuf {
        data_dir.join(Self::FILE_NAME)
    }
}

#[test]
pub fn test_guess_name() {
    let g1 = get_dictname_from_path("/hdd/OpenMdicts/简明英汉汉英词典.1.yaml".to_owned()).unwrap();
    dbg!(&g1);
    assert_eq!(g1, "简明英汉汉英词典".to_owned());
    let g2 =
        get_dictname_from_path("/hdd/OpenMdicts/汉语大词典(简体精排).34.yaml".to_owned()).unwrap();
    dbg!(&g2);
    let g3 = get_dictname_from_path(
        "/hdd/OpenMdicts/柯林斯COBUILD高阶英汉双解学习词典.4.yaml".to_owned(),
    )
    .unwrap();
    dbg!(&g3);
}

pub fn get_dictname_from_path(path: String) -> Option<String> {
    let pa = PathBuf::from(&path);
    let s = pa.file_stem().unwrap().to_str().unwrap().split_once(".");

    if s.is_none() {
        None
    } else {
        Some(s.unwrap().0.to_owned())
    }
}

pub trait Diverge {
    type Ix;
    fn search(&self, query: &str, num: usize, param: bool) -> Result<Vec<DefItemWrapped>>;
    fn bench(&self, cmd: Commands) -> Result<()> {
        unimplemented!()
    }
}

impl Diverge for offdict<Strprox> {
    type Ix = Strprox;
    fn search(&self, query: &str, num: usize, _: bool) -> Result<Vec<DefItemWrapped>> {
        let cands = self.candidates(query, TopkParam::new(num))?;
        let mut res: Vec<DefItemWrapped> = vec![];
        for s in cands {
            res.push(self.retrieve(s).unwrap());
        }
        Ok(res)
    }
    fn bench(&self, cmd: Commands) -> Result<()> {
        match cmd {
            Commands::bench { delay, short } => {
                let num = 20;
                println!("choosing {} words at random", num);
                let mut rng = thread_rng();
                let strvec = &self.set.as_ref().unwrap().yoke.get().trie.strings;
                dbg!(&strvec[2000..2006]);
                let word = strvec.choose_multiple(&mut rng, num);
                for q in word {
                    if q.len() > 10 {
                        continue;
                    }
                    println!("query \"{}\"", q);
                    self.candidates(&q, TopkParam::new(3))?;
                    if delay {
                        std::thread::sleep(Duration::from_micros(200));
                    }
                }
            }
            _ => unreachable!(),
        }

        Ok(())
    }
}

pub const DBPATH: &str = "dicts.db";

pub fn rmdata<Ix: Indexer>(data: &offdict<Ix>) -> Result<()> {
    let dp = &data.dirpath;
    remove_dir_all(dp)?;

    Ok(())
}

impl<Ix: Indexer> offdict<Ix> {
    pub fn serialize<T: Serialize>(v: &T) -> Result<Vec<u8>, Box<bincode::ErrorKind>> {
        bincode::serialize(v)
    }

    pub fn deserialize<'a, T: Deserialize<'a>>(v: &'a [u8]) -> Result<T, Box<bincode::ErrorKind>> {
        bincode::deserialize(v)
    }

    pub fn open_db(path: PathBuf) -> Result<Self> {
        let db;
        if !path.is_dir() {
            create_dir_all(&path).unwrap();
        }

        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.set_prefix_extractor(SliceTransform::create("pre", |bs| DBKey::slice(bs).0, None));
        let mut tableopts = BlockBasedOptions::default();
        tableopts.set_index_type(rocksdb::BlockBasedIndexType::HashSearch);
        opts.set_block_based_table_factory(&tableopts);

        db = Arc::new(rocks::open(&opts, path.join(DBPATH)).unwrap().into());
        Self::from_db(db, path)
    }

    pub fn from_db(db: Arc<RwLock<rocks>>, path: PathBuf) -> Result<Self> {
        let idx = path.join(Ix::FILE_NAME);
        let od = if idx.exists() {
            offdict {
                db,
                set: Some(Ix::load_file(&idx)?),
                dirpath: path,
                set_input: None,
            }
        } else {
            offdict {
                db,
                set: None,
                dirpath: path,
                set_input: None,
            }
        };

        Ok(od)
    }

    pub fn candidates(&self, query: &str, param: Ix::Param) -> Result<candidates> {
        if let Some(index) = &self.set {
            index.query(query, param)
        } else {
            Ok(Default::default())
        }
    }

    pub fn retrieve(&self, cand: candidate) -> Option<DefItemWrapped> {
        let mut items: BTreeMap<String, def_bin::Def> = BTreeMap::new();
        for res in self
            .db
            .read()
            .unwrap()
            .prefix_iterator(DBKey::from(cand.as_str(), ""))
        {
            if res.is_ok() {
                let (k, v) = res.unwrap();
                items.insert(
                    String::from_utf8(DBKey::slice(&k).1.to_vec()).unwrap(),
                    Self::deserialize(&v).unwrap(),
                );
            }
        }
        if items.len() > 0 {
            Some(def_bin::WrapperDef { items, word: cand })
        } else {
            None
        }
    }

    pub fn export_all_yaml(&self, path: &str) {
        let file = File::create(path).expect("Unable to open file");
        // let mut defs: Vec<DefItemInDB> = vec![];
        let mut flat: Vec<DefItem> = vec![];
        for r in self
            .db
            .read()
            .unwrap()
            .iterator(rocksdb::IteratorMode::Start)
        {
            if r.is_ok() {
                let (k, v) = r.unwrap();
                // let k: DBKey = Self::deserialize(&k).unwrap();
                flat.push(Self::deserialize(&v).unwrap());
            }
        }

        serde_yaml::to_writer(file, &flat).unwrap();
    }

    pub fn import_glob(&self, path: &str) -> Result<()> {
        let options = glob::MatchOptions {
            case_sensitive: false,
            ..Default::default()
        };

        let mut pendin: Vec<(String, String)> = vec![];

        for entry in glob::glob_with(path, options)? {
            let ee = entry?;
            let entr = ee.to_str().unwrap();
            let dict_name = get_dictname_from_path(entr.to_owned());
            if let Some(s) = dict_name {
                println!("import {} as {}", entr, s.as_str());
                pendin.push((entr.to_owned(), s));
            } else {
                bail!("Error getting dict name for {}", entr)
            }
        }
        println!("importing {} files", pendin.len());
        pendin.into_iter().for_each(|(p, e)| {
            self.import_from_file(p.as_str(), e.as_str()).unwrap();
        });
        let stat = self.stat();
        println!("{}", stat);

        self.db.write().unwrap().flush()?;

        Ok(())
    }

    pub fn import_from_file(&self, path: &str, dict_name: &str) -> Result<()> {
        let ds = Def::load_yaml(&path, &dict_name)?;
        debug_println!("loaded {} Defs", ds.len());
        self.import_defs(ds)?;

        Ok(())
    }

    #[timed]
    pub fn import_defs(&self, defs: Vec<DefItem>) -> Result<()> {
        let ws: Vec<DefItemWrapped> = defs.into_iter().map(|d| d.into()).collect();
        self.import_wrapped(ws);
        Ok(())
    }

    pub fn import_wrapped(&self, wrapped: Vec<DefItemWrapped>) {
        for incoming_w in wrapped {
            for (k, v) in incoming_w.items {
                self.db
                    .read()
                    .unwrap()
                    .put(v.key(), Self::serialize(&v).unwrap())
                    .unwrap();
            }
        }
    }

    pub fn stat(&self) -> stat {
        let t = self
            .db
            .read()
            .unwrap()
            .iterator(rocksdb::IteratorMode::Start)
            .count();

        stat {
            words: t,
            unique_words: if let Some(ref ix) = self.set {
                Some(ix.count())
            } else {
                None
            },
        }
    }

    #[timed]
    pub fn build_index_from_db(&mut self, txt: bool) -> Result<usize> {
        let mut px = self.dirpath.clone();
        px.push(if txt { "words.txt" } else { Ix::FILE_NAME });

        let mut set: BTreeSet<String> = BTreeSet::new();
        for res in self
            .db
            .read()
            .unwrap()
            .iterator(rocksdb::IteratorMode::Start)
        {
            if res.is_ok() {
                let (k, v) = res.unwrap();
                let word = String::from_utf8(DBKey::slice(&k).0.to_vec()).unwrap();
                set.insert(word);
            }
        }
        let c = set.len();

        debug_println!("word set len {}", set.len());
        let mut sorted: Vec<String> = set.into_iter().collect();
        sorted.sort();

        if txt {
            let k = sorted.join("\n");
            File::create(&px)?.write_all(k.as_bytes())?;
            println!("written to {:?}", &px);
            process::exit(0);
        } else {
            Ix::build_all(sorted, &px)?;
            self.set = Some(Ix::load_file(&px)?);
        }

        Ok(c)
    }
}

// Result of yaml checking
#[derive(Serialize)]
struct DefCheck {
    def: Def,
    has_non_empty_alternative: bool,
}

impl<'a> AnyDef<'a, Self> for Def {
    fn load_yaml(path: &str, name: &str) -> Result<Vec<DefItem>> {
        let file = File::open(path).expect("Unable to open file");
        let mut yaml_defs: Vec<Def> = serde_yaml::from_reader(file)?;
        for def in yaml_defs.iter_mut() {
            (*def).dictName = Some(name.to_owned());
        }

        let vec_d: Vec<DefItem> = yaml_defs.into_iter().map(|x| x.into()).collect();

        Ok(vec_d)
    }

    fn check_yaml_defs(
        imported_Defs: Vec<Def>,
        save: bool,
        mut unused: BTreeSet<String>,
        path: &str,
    ) {
        let imported_words: BTreeSet<&str>;

        imported_words = BTreeSet::from_iter(
            imported_Defs
                .iter()
                .map(|x| x.word.as_ref().unwrap().as_str()),
        );

        // Outputs a cleaned up source, best effort
        if save {
            let empty_Defs: Vec<Def>;
            let mut non_empty_Defs: Vec<Def>;
            empty_Defs = imported_Defs
                .iter()
                .filter(|d| d.empty_())
                .map(|d| d.clone())
                .collect();
            non_empty_Defs = imported_Defs
                .iter()
                .filter(|d| !d.empty_())
                .map(|d| d.clone())
                .collect();

            let mut check_res: Vec<DefCheck> = vec![];
            let mut with_alt: usize = 0;
            for d in empty_Defs.iter() {
                let c = imported_words.contains::<str>(d.word.as_ref().unwrap().as_str());
                check_res.push(DefCheck {
                    def: d.clone(),
                    has_non_empty_alternative: c,
                });
                // Empty defs with an alternative can be removed without pity
                // Results that are not factually empty means the code has to be revised.
                if c {
                    with_alt += 1;
                }
            }

            let mut imported_words_m: collections::BTreeMap<String, bool> =
                collections::BTreeMap::new();

            let unique_Defs: Vec<Def> = non_empty_Defs
                .into_iter()
                .filter(|x| {
                    if *imported_words_m
                        .get::<str>(x.word.as_ref().unwrap().as_ref())
                        .unwrap_or(&false)
                    {
                        false
                    } else {
                        imported_words_m.insert(x.word.as_ref().unwrap().clone(), true);
                        true
                    }
                })
                .collect();

            println!(
                "Num empty {}, num total {}, empty but with alt {}",
                empty_Defs.len(),
                imported_Defs.len(),
                with_alt
            );

            let cleaned: Vec<Def> = unique_Defs.into_iter().map(|i| i.cleanup()).collect();
            let mut pb = PathBuf::from_str(path).unwrap();
            let fname = pb.file_name().unwrap().to_os_string();
            pb.pop();
            pb.push("checked");
            create_dir_all(&pb).unwrap();

            let f_processed = File::create(pb.join(fname.clone())).unwrap();
            let f_empty =
                File::create(pb.join(fname.to_str().unwrap().replace(".yaml", ".e.yaml"))).unwrap();

            serde_yaml::to_writer(f_empty, &check_res).unwrap();
            serde_yaml::to_writer(f_processed, &cleaned).unwrap();
        }
        println!("{:?}", unused);
    }

    // Validates the old def sources are correctly parsed by serde, and converted
    // Empty defs were caused by buggy .mdx parsers. They are reversed-engineered, anyway
    fn check_yaml(path: &str, save: bool) {
        let file = File::open(path).expect("Unable to open file");
        let d = serde_yaml::Deserializer::from_reader(file);
        let mut unused = BTreeSet::new();

        Self::check_yaml_defs(
            serde_ignored::deserialize(d, |path| {
                unused.insert(path.to_string());
            })
            .unwrap(),
            save,
            unused,
            path,
        )
    }
}

// To import DefNew from
pub trait AnyDef<'a, T: Deserialize<'a>> {
    fn load_yaml(path: &str, name: &str) -> Result<Vec<DefItem>>;

    fn check_yaml(path: &str, save: bool);
    fn check_yaml_defs(imported_Defs: Vec<Def>, save: bool, unused: BTreeSet<String>, path: &str);
}

// store in database as wrapped
// unwrapped in sources
pub fn flatten(wr: Vec<DefItemWrapped>) -> Vec<DefItem> {
    let mut res = Vec::new();
    for wrapper in wr.into_iter() {
        for (_, d) in wrapper.items.into_iter() {
            res.push(d)
        }
    }

    res
}

pub fn flatten_human(wr: Vec<DefItemWrapped>) -> Vec<Def> {
    let mut res = Vec::new();
    for wrapper in wr.into_iter() {
        res.extend(wrapper.vec_human())
    }
    res
}

impl Def {
    pub fn normalize_def(mut self) -> Self {
        if self.groups.is_some() {
            self.definitions = self.groups;
            self.groups = None;
        }
        if self.definitions.is_some() {
            self.definitions = Some(
                self.definitions
                    .unwrap()
                    .into_iter()
                    .map(|x| x.normalize_def())
                    .collect(),
            );
        }
        // d = recursive_path_shorten(d);
        self
    }

    fn normalize_def_ref(&mut self) {
        if self.groups.is_some() {
            std::mem::swap(&mut self.definitions, &mut self.groups);
            // let x = self.groups;
            self.groups = None;
        }
    }
}

use clap::{Parser, Subcommand};
#[allow(non_camel_case_types)]
#[derive(Debug, Subcommand)]
pub enum Commands {
    #[command(
        about = "Import definitions from an yaml file",
        arg_required_else_help = false
    )]
    yaml {
        /// Glob pattern
        #[arg(short = 'p', required = true)]
        path: String,
        #[arg(short = 'c', long)]
        check: bool,
        #[arg(short = 's', long)]
        save: bool,
    },
    #[command(about = "Stats")]
    stat {},
    #[command(about = "Fuzzy query (prefix)")]
    lookup {
        query: String,
    },
    // TODO: bincode import
    // #[command(about = "Convert an yaml file to cbor")]
    // cbor {
    //     // Converts a yaml to cbor and save it.
    //     #[arg(short = 'p')]
    //     path: String,
    //     #[arg(short = 'n')]
    //     name: String, // Name to be displayed
    // },
    reset {},
    #[command(about = "Build index, required after adding or removing words")]
    build {
        /// Don't build an index. Instead, export all entries into a txt
        #[arg(short = 'e')]
        export: bool,
    },
    #[command(about = "Benchmark")]
    bench {
        #[arg(short, long)]
        delay: bool,
        #[arg(short, long)]
        short: bool,
    },
}

#[derive(Parser, Debug)]
#[command(about = "Offline dictionary", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

pub fn process_cmd<'a, D: Indexer>(db: impl FnOnce() -> Result<&'a mut offdict<D>>) -> Result<bool>
where
    offdict<D>: Diverge,
{
    let args = Cli::parse();

    match args.command {
        Some(Commands::yaml { path, check, save }) => {
            if check {
                let options = glob::MatchOptions {
                    case_sensitive: false,
                    ..Default::default()
                };

                for entry in glob::glob_with(&path, options)? {
                    let entr = entry?;
                    println!("checking {}", entr.to_str().unwrap());
                    Def::check_yaml(entr.to_str().unwrap(), save);
                }

                return Ok(false);
            } else {
                match db()?.import_glob(&path) {
                    Ok(()) => println!("imported"),
                    Err(e) => println!("{:?}", e),
                }
            }
            Ok(false)
        }
        Some(Commands::stat {}) => {
            let s = db()?.stat();
            println!("{}", s);
            Ok(false)
        }
        Some(Commands::lookup { query }) => {
            for d in db()?.search(&query, 1, true)? {
                let list: Vec<Def> = d.vec_human();
                println!("{}", serde_yaml::to_string::<Vec<Def>>(&list)?)
            }
            Ok(false)
        }
        Some(Commands::reset {}) => {
            rmdata(db()?)?;
            println!("reset.");
            Ok(false)
        }
        Some(Commands::build { export }) => {
            let c = db()?.build_index_from_db(export)?;
            println!("built, {} words", c);
            Ok(true)
        }
        None => Ok(true),
        Some(keep) => {
            let db = db()?;
            match &keep {
                Commands::bench { .. } => {
                    db.bench(keep)?;
                }
                _ => (),
            }
            Ok(true)
        }
    }
}

#[test]
fn test_worse_case() -> Result<()> {
    let case = "bring more land under cultivation";
    let conf = crate::config::get_config();
    let db_path = PathBuf::from(conf.data_path.clone());
    let db = offdict::<Strprox>::open_db(db_path)?;
    println!("testing");
    db.search(case, 3, false)?;
    Ok(())
}

use std::sync::{Arc, RwLock};
use tokio::{self};
use warp::Filter;

pub static mut DB: Option<offdict<Strprox>> = None;

pub fn init_db(db_path: PathBuf) -> Result<&'static mut offdict<Strprox>> {
    if let Some(_o) = unsafe { &DB } {
    } else {
        unsafe { DB = Some(offdict::<Strprox>::open_db(db_path)?) };
    }
    Ok(unsafe { DB.as_mut() }.unwrap())
}

pub mod config;

#[derive(Serialize, Deserialize)]
pub struct Stat {
    words: u64,
}

#[derive(Deserialize, Default)]
pub struct ApiOpts {
    expensive: bool,
}

#[derive(Deserialize, Default, Serialize)]
pub struct SetRes;

pub async fn serve<Ix: Indexer + Send + Sync + 'static>(db: &'static offdict<Ix>) -> Result<()>
where
    offdict<Ix>: Diverge,
{
    let lookup = warp::get()
        .and(warp::path("q"))
        .and(warp::path::param::<String>())
        .and(
            warp::query::<ApiOpts>()
                .map(Some)
                .or_else(|_| async { Ok::<(Option<ApiOpts>,), std::convert::Infallible>((None,)) }),
        )
        .map(move |word: String, opts: Option<ApiOpts>| {
            let word = percent_encoding::percent_decode_str(&word)
                .decode_utf8()
                .unwrap()
                .to_string();
            warp::reply::json(&api_q(&db, &word, opts.unwrap_or_default()).unwrap())
        });

    let stat = warp::get()
        .and(warp::path("stat"))
        .map(|| warp::reply::json(&Stat { words: 0 }));

    let set = warp::get()
        .and(warp::path("set"))
        .and(warp::path::param::<String>())
        .and(
            warp::query::<ApiOpts>()
                .map(Some)
                .or_else(|_| async { Ok::<(Option<ApiOpts>,), std::convert::Infallible>((None,)) }),
        )
        .map(move |word: String, opts: Option<ApiOpts>| {
            let word = percent_encoding::percent_decode_str(&word)
                .decode_utf8()
                .unwrap()
                .to_string();
            // let mut r: SetRes = SetRes { defs: false };
            if db.set_input.is_some() {
                db.set_input.unwrap()(word, false).unwrap();
            }
            warp::reply::json(&SetRes)
        });

    println!("API listening on :3030");
    Ok(warp::serve(lookup.or(stat).or(set))
        .run(([0, 0, 0, 0], 3030)) // XXX: this has to be hard coded, who cares
        .await)
}

pub async fn repl<Ix: Indexer>(db: &offdict<Ix>) -> Result<()>
where
    offdict<Ix>: Diverge,
{
    loop {
        let li = readline().await.unwrap();
        let li = li.trim();
        if li.is_empty() {
            continue;
        } else {
            match respond(li, db) {
                Ok(quit) => {
                    if quit {
                        break Ok(());
                    }
                }
                Err(err) => {
                    write!(std::io::stdout(), "{}", err)
                        .map_err(|e| e.to_string())
                        .unwrap();
                    std::io::stdout()
                        .flush()
                        .map_err(|e| e.to_string())
                        .unwrap();
                }
            }
        }
    }
}

pub async fn readline() -> Result<String> {
    let mut out = tokio::io::stdout();
    out.write_all("# ".purple().to_string().as_bytes()).await?;
    out.flush().await?;
    let mut buffer = Vec::new();
    tokio::io::stdin().read(&mut buffer).await?;
    let stdin = tokio::io::stdin();
    let reader = tokio::io::BufReader::new(stdin);
    let mut lines = tokio::io::AsyncBufReadExt::lines(reader);
    Ok(lines.next_line().await?.unwrap())
}

pub fn api_q<Ix: Indexer>(db: &offdict<Ix>, query: &str, opts: ApiOpts) -> Result<Vec<Def>>
where
    offdict<Ix>: Diverge,
{
    println!("\nq: {}", query);

    let mut arr = db.search(query, 30, opts.expensive)?;
    let mut def_list = flatten_human(arr);

    Ok(def_list)
}

fn respond<Ix: Indexer>(line: &str, db: &offdict<Ix>) -> Result<bool>
where
    offdict<Ix>: Diverge,
{
    let mut arr = db.search(line, 2, true)?;

    println!("{} results", arr.len());
    arr.truncate(2);
    for d in arr.into_iter() {
        println!(
            "{}",
            serde_yaml::to_string::<Vec<Def>>(&d.vec_human()).unwrap()
        );
    }

    Ok(false)
}

pub use def::*;

use crate::topk::TopkParam;
pub mod def;
mod tests;

#[cfg(feature = "fst")]
pub mod fst_index;

pub mod def_bin;
