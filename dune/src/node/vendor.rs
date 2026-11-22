include!(concat!(env!("OUT_DIR"), "/lib.rs"));

pub struct Lib;

#[j8::object("lib")]
impl Lib {
    fn source(id: &str) -> Option<&'static str> {
        LIB.binary_search_by(|(known, _)| known.cmp(&id)).ok().map(|i| LIB[i].1)
    }

    fn ids() -> Vec<&'static str> {
        LIB.iter().map(|(id, _)| *id).collect()
    }
}
