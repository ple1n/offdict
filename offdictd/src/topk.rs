use std::sync::Mutex;

use anyhow::{anyhow, Result};
use metacomplete::prefix::meta::Cache;
use metacomplete::{MetaAutocompleter, TreeStringT};
use yoke::{Yoke, Yokeable};

use crate::*;
use crate::{candidates, Indexer};

pub struct Strprox {
    pub yoke: Yoke<MetaAutocompleter<'static>, Mmap>,
    pub cache: Mutex<Cache<'static>>,
}

#[derive(new)]
pub struct TopkParam {
    num: usize,
}

impl Indexer for Strprox {
    const FILE_NAME: &'static str = "strprox";
    type Param = TopkParam;
    #[timed]
    fn build_all(words: impl IntoIterator<Item = String>, pp: &std::path::Path) -> Result<()> {
        let arr: Vec<_> = words
            .into_iter()
            .map(|k| TreeStringT::from_owned(k))
            .collect();
        let set = MetaAutocompleter::new(arr.len(), arr);
        let mut fw = std::fs::File::create(pp)?;
        bincode::serialize_into(&mut fw, &set)?;
        Ok(())
    }
    #[timed]
    fn load_file(pp: &std::path::Path) -> Result<Self> {
        println!("loading index from {:?}", pp);
        let f = std::fs::File::open(pp)?;
        let sel = Self {
            yoke: Yoke::try_attach_to_cart(unsafe { Mmap::map(&f) }?, |data| {
                bincode::deserialize(data)
            })?,
            cache: Default::default(),
        };
        println!("index loaded");
        Ok(sel)
    }
    #[timed]
    fn query(&self, query: &str, param: TopkParam) -> Result<crate::candidates> {
        let mut lk = self.cache.lock().unwrap();
        let topk = self.yoke.get();
        let num = 10;
        let mut rx = topk.threshold_top_k(query, num, 3, &mut lk);
        dbg!(&rx[..min(2, rx.len())]);
        rx.truncate(num);
        let cands: Vec<_> = rx.into_iter().map(|k| k.string).collect();
        Ok(cands)
    }
    fn count(&self) -> usize {
        self.yoke.get().len()
    }
}
