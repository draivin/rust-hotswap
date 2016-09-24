 #![feature(arc_counts)]

pub extern crate libloading;
pub extern crate parking_lot;

use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, Ordering};
use libloading::Library;

// Those are the containers used for the imported libraries.
trait FnArc: Send + Sync {
    fn ref_count(&self) -> usize;
}

impl<T: Send + Sync> FnArc for Arc<T> {
    fn ref_count(&self) -> usize {
        Arc::strong_count(self)
    }
}

pub struct RefManager {
    lib: AtomicPtr<Library>,
    // Contains a copy of every arc to a function in this library
    // which is no longer stored in the global pointers.
    refs: Vec<Box<FnArc>>
}

impl RefManager {
    pub fn new(lib: Library) -> Self {
        let ptr = Box::into_raw(Box::new(lib));
        let ptr = AtomicPtr::new(ptr);

        RefManager {
            lib: ptr,
            refs: Vec::new()
        }
    }

    pub fn should_drop(&mut self) -> bool {
        for i in (0..self.refs.len()).rev() {
            if self.refs[i].ref_count() == 1 {
                self.refs.remove(i);
            } else {
                return false;
            }
        }
        true
    }

    pub fn add_ref<T: 'static + Send + Sync>(&mut self, arc: Arc<T>) {
        self.refs.push(Box::new(arc));
    }
}

impl Drop for RefManager {
    fn drop(&mut self) {
        unsafe {
            Box::from_raw(self.lib.load(Ordering::Relaxed));
        }
    }
        }
