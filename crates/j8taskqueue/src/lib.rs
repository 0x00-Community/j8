#![no_std]
extern crate alloc;

mod node;
mod queue;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;

pub use node::TaskId;
pub use queue::{Queue, TaskQueue};

pub type Job<'t> = Box<dyn FnOnce() + Send + 't>;

pub trait Pool: Sync {
    fn threads(&self) -> usize;

    fn run(&self, jobs: Vec<Job<'_>>);
}

pub type Task<'a> = Box<dyn ITask<'a> + 'a>;

pub trait ITask<'a>: Send {
    fn recursable(&self) -> bool;

    fn depend(&self) -> &[TaskId<'a>] {
        &[]
    }

    fn exec(&mut self, poll: &mut dyn ITaskPoll<'a>, quota: &mut Quota) -> Flow;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Done,
    Again,
    Yield,
}

#[derive(Debug)]
pub struct Quota {
    left: usize,
}

impl Quota {
    pub const UNLIMITED: usize = usize::MAX;

    pub fn left(&self) -> usize {
        self.left
    }

    pub fn spend(&mut self, n: usize) -> usize {
        let n = n.min(self.left);
        self.left -= n;
        n
    }

    pub fn exhausted(&self) -> bool {
        self.left == 0
    }
}

impl From<usize> for Quota {
    fn from(left: usize) -> Self {
        Self { left }
    }
}

pub trait ITaskPoll<'a> {
    fn add(&mut self, task: Task<'a>) -> Result<TaskId<'a>, Refused<'a>>;

    fn run(&mut self) -> Result<Ran, Fault>;
}

pub struct Refused<'a>(pub Task<'a>);

impl fmt::Debug for Refused<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Refused: a non-recursable task cannot add tasks")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Fault {
    Reentrant = 1,
    NotRecursable = 2,
    Unspent = 3,
}

impl Fault {
    const ALL: [Self; 3] = [Self::Reentrant, Self::NotRecursable, Self::Unspent];
}

impl From<Fault> for u8 {
    fn from(f: Fault) -> Self {
        f as u8
    }
}

impl TryFrom<u8> for Fault {
    type Error = u8;

    fn try_from(code: u8) -> Result<Self, u8> {
        Self::ALL.into_iter().find(|f| u8::from(*f) == code).ok_or(code)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stop {
    #[default]
    Empty,
    Budget,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ran {
    pub execs: usize,
    pub spent: usize,
    pub stop: Stop,
}
