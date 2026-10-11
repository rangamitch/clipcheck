//! Reusable pixel buffers shared between the WGC readback and the pacer.
//!
//! Each published frame is an `Arc<Vec<u8>>`. The readback writes the next frame into a slot
//! whose buffer nothing else references any more, so the steady state allocates nothing.

use std::sync::Arc;

/// A fixed-size ring of byte buffers. Only the readback side uses it, so no locking is needed.
#[derive(Debug)]
pub(crate) struct BufferRing {
    slots: Vec<Arc<Vec<u8>>>,
    cap: usize,
    next_evict: usize,
    leased: usize,
}

impl BufferRing {
    /// Creates a ring that keeps at most `cap` buffers (`cap` must be at least 1).
    pub(crate) fn new(cap: usize) -> Self {
        assert!(cap > 0, "BufferRing needs at least one slot");
        BufferRing {
            slots: Vec::with_capacity(cap),
            cap,
            next_evict: 0,
            leased: 0,
        }
    }

    /// Returns a writable buffer for the next frame.
    ///
    /// Reuses the first slot that no other owner holds. If every slot is shared and the ring
    /// is not full, a new slot is added. If it is full, the next slot in round-robin order is
    /// replaced; the old buffer lives on for whoever still holds it.
    pub(crate) fn lease(&mut self) -> &mut Vec<u8> {
        let idx = match self
            .slots
            .iter()
            .position(|s| Arc::strong_count(s) == 1 && Arc::weak_count(s) == 0)
        {
            Some(i) => i,
            None if self.slots.len() < self.cap => {
                self.slots.push(Arc::new(Vec::new()));
                self.slots.len() - 1
            }
            None => {
                let i = self.next_evict;
                self.next_evict = (i + 1) % self.cap;
                self.slots[i] = Arc::new(Vec::new());
                i
            }
        };
        self.leased = idx;
        // Unique by construction above, so this never clones.
        Arc::make_mut(&mut self.slots[idx])
    }

    /// Shared handle to the buffer returned by the most recent [`BufferRing::lease`].
    ///
    /// Must be called after a `lease`.
    pub(crate) fn published(&self) -> Arc<Vec<u8>> {
        Arc::clone(&self.slots[self.leased])
    }

    #[cfg(test)]
    fn slot_count(&self) -> usize {
        self.slots.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn released_buffer_is_reused_with_its_allocation() {
        let mut ring = BufferRing::new(3);
        let first_ptr = {
            let buf = ring.lease();
            buf.resize(64, 7);
            buf.as_ptr()
        };
        // Publish, then let every outside handle go.
        drop(ring.published());
        let buf = ring.lease();
        assert_eq!(buf.as_ptr(), first_ptr, "unshared slot should be reused");
        assert_eq!(buf.len(), 64, "contents are kept until overwritten");
        assert_eq!(ring.slot_count(), 1, "no new slot needed");
    }

    #[test]
    fn shared_buffer_is_never_handed_out_again() {
        let mut ring = BufferRing::new(3);
        ring.lease().resize(16, 1);
        let held = ring.published();
        let held_ptr = held.as_ptr();
        let next = ring.lease();
        assert_ne!(
            next.as_ptr(),
            held_ptr,
            "a buffer still held must not be leased"
        );
        next.resize(16, 2);
        assert_eq!(held.as_slice(), &[1u8; 16][..], "held frame stays intact");
    }

    #[test]
    fn ring_size_is_bounded() {
        let mut ring = BufferRing::new(2);
        let held: Vec<Arc<Vec<u8>>> = (0..5)
            .map(|i| {
                let buf = ring.lease();
                buf.resize(8, i as u8);
                ring.published()
            })
            .collect();
        assert!(ring.slot_count() <= 2, "ring must not grow past its cap");
        // Every published frame keeps its own contents.
        for (i, h) in held.iter().enumerate() {
            assert_eq!(h[0], i as u8);
        }
    }
}
