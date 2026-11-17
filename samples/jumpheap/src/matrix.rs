//! Matrix memory in public heaps, handed to JS as `ArrayBuffer`s.
//!
//! Every jumpheap process owns one paragc public heap: a memory file in `/dev/shm` that any process can map. A
//! matrix's memory lives there: a table of rows, each row one object of [`COLS`] numbers (an object is at most 128
//! MiB, so a matrix is cut by rows). This side only allocates rows and finds them; the matrix itself is a JS object
//! (the prelude's `Matrix`) whose rows are `Float64Array`s over those rows, and everything computed on it is computed
//! in JS.
//!
//! A worker that receives another process's matrix takes a new entry in that process's heap (its root table): the JS
//! `Matrix` lives in the receiver's isolate heap and references the other heap, which keeps the rows alive for as
//! long as any of their `ArrayBuffer`s lives. Once the isolate's collector frees the last of them, the entry goes.
use std::io;
use std::num::NonZeroUsize;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use j8::frame::{Agent, Error, ErrorKind, Property, Value};
use j8::lockfree::{Chain, Oneshot};
use j8::{ArrayBuffer, Backing, Class, External, Glue, IntoJs};
use paragc::{Public, PublicHeap, PublicMutator, PublicSlot, PublicSpace, Shared, public_brand};

/// Numbers in a row
pub const COLS: usize = 100_000;
/// Rows a matrix can have
pub const MAX_ROWS: usize = 10_000;
/// Collector threads of a public heap
const HEAP_THREADS: NonZeroUsize = NonZeroUsize::new(4).expect("non-zero");
/// Stack of the threads that allocate rows: a row is put together on the stack before it moves into the heap
const STACK: usize = 8 << 20;

/// One row: `f64`s as JS writes them (atomic cells: JS in several processes reads and writes them in place)
#[repr(C)]
#[derive(Shared)]
pub struct Row {
    cells: [AtomicU64; COLS],
}

/// A matrix's memory: how many rows it has, and the rows
#[repr(C)]
#[derive(Shared)]
pub struct Table<'p> {
    rows: u64,
    row: [PublicSlot<'p, Row>; MAX_ROWS],
}

public_brand!(TableB = for<'p> Table<'p>);

/// The public heaps of this process: its own, and the ones it opened
pub struct Space {
    addresses: PublicSpace,
    path: PathBuf,
    mine: Arc<PublicHeap>,
    peers: Chain<(PathBuf, Arc<PublicHeap>)>,
}

impl Space {
    /// Reserves the public address range and creates this process's heap at `path` (it dies with the process)
    pub fn create(path: PathBuf) -> io::Result<Arc<Self>> {
        let addresses = PublicSpace::reserve()?;
        let mine = Arc::new(addresses.create(&path, HEAP_THREADS)?);
        Ok(Arc::new(Self { addresses, path, mine, peers: Chain::default() }))
    }

    /// The heap at `path`, mapped once
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

/// A matrix's table, held from this process: the heap it is in (mapped while this lives) and an entry there
struct Held {
    heap: Arc<PublicHeap>,
    path: PathBuf,
    table: Public<TableB>,
}

impl Held {
    /// A table of `rows` zero rows in `heap`, the rows allocated on every CPU at once
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

    /// `f` on slices of `0..rows`, one per CPU, at once
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

/// A matrix's memory on its way to JS: where it is, and the address of every row
pub struct Rows {
    held: Arc<Held>,
    at: Vec<usize>,
}

impl Rows {
    /// The rows of `held`'s table, read inside its heap
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
    /// `{ path, index, rows }`: where the matrix is (what another process takes it by) and an `ArrayBuffer` over
    /// every row, each keeping the entry (and so the rows) alive
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        let path = self.held.path.display().to_string().into_js(a)?;
        let index = self.held.table.index().into_js(a)?;
        let buffers = self
            .at
            .iter()
            .map(|&p| {
                let owner: Box<dyn Send> = Box::new(Arc::clone(&self.held));
                // SAFETY: a row is a large object (over paragc's 16 KiB limit) and large objects never move; it stays
                // alive while the table holding it is rooted (`owner` holds the entry) and mapped (`owner` holds the
                // heap); its cells are atomics, so writing them through this pointer is allowed
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

/// What a job off the event loop gives back: a value, or an error thrown in JS
struct Done<T>(Result<T, String>);

impl<'gc, T: IntoJs<'gc>> IntoJs<'gc> for Done<T> {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        match self.0 {
            Ok(v) => v.into_js(a),
            Err(e) => Err(a.throw(ErrorKind::Error, &e)),
        }
    }
}

/// `__j8_native.heaps`: this process's public heaps
pub struct Heaps {
    space: Arc<Space>,
}

#[j8::class]
impl Heaps {
    /// This process's heap file
    #[js(getter)]
    fn path(&self) -> String {
        self.space.path.display().to_string()
    }

    /// A promise of a new matrix's memory in this process's heap: `rows` rows of [`COLS`] zeros (allocated on every
    /// CPU at once, off the event loop)
    fn allocate<'gc>(&self, a: &mut Agent<'gc, '_>, rows: u32) -> Result<Value<'gc>, Error> {
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

    /// The matrix memory another process sent: entry `index` of the heap at `path` (mapped now if it isn't yet)
    fn take(&self, a: &mut Agent<'_, '_>, path: String, index: u32) -> Result<Rows, Error> {
        let heap = self.space.heap(Path::new(&path)).map_err(|e| a.throw(ErrorKind::Error, &format!("{path}: {e}")))?;
        let found = heap.try_mutate(|m| m.find::<TableB>(index));
        let Ok(Some(table)) = found else { return Err(a.throw(ErrorKind::Error, &format!("{path}: no matrix at entry {index}"))) };
        Rows::of(Held { heap, path: PathBuf::from(path), table }).map_err(|e| a.throw(ErrorKind::Error, &e))
    }

    /// Collects this process's heap now (what other processes hold stays)
    fn collect(&self) {
        self.space.mine.collect();
    }
}

impl Heaps {
    /// Gives this runtime its `__j8_native.heaps`
    pub fn attach(a: &mut Agent<'_, '_>, space: Arc<Space>) {
        let natives = Glue::natives(a);
        let heaps = <Self as Class>::install(a, natives).instance(a, Self { space });
        natives.borrow_mut(a.mc).define("heaps", Property::hidden(heaps.into()));
    }
}
