use std::io;
use std::num::NonZeroUsize;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use j8::frame::{Scope, Error, ErrorKind, Property, Value};
use j8::lockfree::{Chain, Oneshot};
use j8::{ArrayBuffer, Backing, Class, External, Glue, IntoJs};
use paragc::{Public, PublicHeap, PublicMutator, PublicSlot, PublicSpace, Shared, public_brand};

pub const COLS: usize = 100_000;

pub const MAX_ROWS: usize = 10_000;

const HEAP_THREADS: NonZeroUsize = NonZeroUsize::new(4).expect("non-zero");

const STACK: usize = 8 << 20;

#[repr(C)]
#[derive(Shared)]
pub struct Row {
    cells: [AtomicU64; COLS],
}

#[repr(C)]
#[derive(Shared)]
pub struct Table<'p> {
    rows: u64,
    row: [PublicSlot<'p, Row>; MAX_ROWS],
}

public_brand!(TableB = for<'p> Table<'p>);

pub struct Space {
    addresses: PublicSpace,
    path: PathBuf,
    mine: Arc<PublicHeap>,
    peers: Chain<(PathBuf, Arc<PublicHeap>)>,
}

impl Space {
    pub fn create(path: PathBuf) -> io::Result<Arc<Self>> {
        let addresses = PublicSpace::reserve()?;
        let mine = Arc::new(addresses.create(&path, HEAP_THREADS)?);
        Ok(Arc::new(Self { addresses, path, mine, peers: Chain::default() }))
    }

    fn heap(&self, path: &Path) -> io::Result<Arc<PublicHeap>> {
        if path == self.path {
            return Ok(Arc::clone(&self.mine));
        }
        if let Some((_, h)) = self.peers.iter().find(|(p, _)| p == path) {
            return Ok(Arc::clone(h));
        }
        let h = Arc::new(self.addresses.open(path, HEAP_THREADS)?);
        Ok(Arc::clone(&self.peers.push((path.to_owned(), h)).1))
    }
}

struct Held {
    heap: Arc<PublicHeap>,
    path: PathBuf,
    table: Public<TableB>,
}

impl Held {
    fn allocate(heap: Arc<PublicHeap>, path: PathBuf, rows: usize) -> Self {
        let table: Public<TableB> = heap.mutate(|m| m.root::<TableB>(m.alloc(Table { rows: rows as u64, row: core::array::from_fn(|_| PublicSlot::default()) })));
        Self::parallel(rows, |range| {
            heap.mutate(|m| {
                let t = table.get(m).get();
                for i in range {
                    t.row[i].set(m, Some(m.alloc(Row { cells: core::array::from_fn(|_| AtomicU64::new(0)) })));
                }
            });
        });
        Self { heap, path, table }
    }

    fn parallel(rows: usize, f: impl Fn(Range<usize>) + Sync) {
        let threads = std::thread::available_parallelism().map_or(1, NonZeroUsize::get).min(rows.max(1));
        let chunk = rows.div_ceil(threads);
        std::thread::scope(|s| {
            let f = &f;
            for t in 0..threads {
                std::thread::Builder::new().stack_size(STACK).spawn_scoped(s, move || f(t * chunk..((t + 1) * chunk).min(rows))).expect("a worker thread");
            }
        });
    }
}

pub struct Rows {
    held: Arc<Held>,
    at: Vec<usize>,
}

impl Rows {
    fn of(held: Held) -> Result<Self, String> {
        let at = held.heap.try_mutate(|m| Self::addresses(&held.table, m)).map_err(|_| format!("the heap at {} died with its process", held.path.display()))?;
        Ok(Self { held: Arc::new(held), at })
    }

    fn addresses(table: &Public<TableB>, m: &PublicMutator<'_>) -> Vec<usize> {
        let t = table.get(m).get();
        let rows = usize::try_from(t.rows).unwrap_or(0).min(MAX_ROWS);
        t.row[..rows].iter().filter_map(|r| r.get(m)).map(|r| r.as_ptr().expose_provenance()).collect()
    }
}

impl<'gc> IntoJs<'gc> for Rows {
    fn into_js(self, a: &mut Scope<'gc, '_>) -> Result<Value<'gc>, Error> {
        let path = self.held.path.display().to_string().into_js(a)?;
        let index = self.held.table.index().into_js(a)?;
        let buffers = self
            .at
            .iter()
            .map(|&p| {
                let owner: Box<dyn Send> = Box::new(Arc::clone(&self.held));
                ArrayBuffer(Backing::External(unsafe { External::from_raw(std::ptr::with_exposed_provenance_mut(p), size_of::<Row>(), owner) }))
            })
            .collect::<Vec<_>>()
            .into_js(a)?;
        let o = a.object();
        for (key, v) in [("path", path), ("index", index), ("rows", buffers)] {
            o.borrow_mut(a.mc).define(key, Property::data(v));
        }
        Ok(o.into())
    }
}

struct Done<T>(Result<T, String>);

impl<'gc, T: IntoJs<'gc>> IntoJs<'gc> for Done<T> {
    fn into_js(self, a: &mut Scope<'gc, '_>) -> Result<Value<'gc>, Error> {
        match self.0 {
            Ok(v) => v.into_js(a),
            Err(e) => Err(a.throw(ErrorKind::Error, &e)),
        }
    }
}

#[derive(j8::Trace)]
pub struct Heaps {
    #[collect(require_static)]
    space: Arc<Space>,
}

#[j8::class]
impl Heaps {
    #[js(getter)]
    fn path(&self) -> String {
        self.space.path.display().to_string()
    }

    fn allocate<'gc>(&self, a: &mut Scope<'gc, '_>, rows: u32) -> Result<Value<'gc>, Error> {
        let rows = rows as usize;
        if rows == 0 || rows > MAX_ROWS {
            return Err(a.throw(ErrorKind::Range, &format!("a matrix has 1 to {MAX_ROWS} rows")));
        }
        let (heap, path) = (Arc::clone(&self.space.mine), self.space.path.clone());
        let (tx, rx) = Oneshot::new();
        std::thread::Builder::new()
            .stack_size(STACK)
            .spawn(move || tx.send(Done(Rows::of(Held::allocate(heap, path, rows)))))
            .map_err(|e| a.throw(ErrorKind::Error, &format!("spawn: {e}")))?;
        Glue::spawn(a, async move { Glue::settle(rx.await) })
    }

    fn take(&self, a: &mut Scope<'_, '_>, path: String, index: u32) -> Result<Rows, Error> {
        let heap = self.space.heap(Path::new(&path)).map_err(|e| a.throw(ErrorKind::Error, &format!("{path}: {e}")))?;
        let found = heap.try_mutate(|m| m.find::<TableB>(index));
        let Ok(Some(table)) = found else { return Err(a.throw(ErrorKind::Error, &format!("{path}: no matrix at entry {index}"))) };
        Rows::of(Held { heap, path: PathBuf::from(path), table }).map_err(|e| a.throw(ErrorKind::Error, &e))
    }

    fn collect(&self) {
        self.space.mine.collect();
    }
}

impl Heaps {
    pub fn attach(a: &mut Scope<'_, '_>, space: Arc<Space>) {
        let natives = Glue::natives(a);
        let heaps = <Self as Class>::install(a, natives).instance(a, Self { space });
        natives.borrow_mut(a.mc).define("heaps", Property::hidden(heaps.into()));
    }
}
