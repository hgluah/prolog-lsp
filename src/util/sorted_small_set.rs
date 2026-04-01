use core::{cmp::Ordering, fmt, marker::PhantomData, ops::Deref};
use std::iter::Peekable;

use smallvec::{Array, SmallVec, smallvec};
use sorted_iter::{
    SortedIterator, assume::AssumeSortedByItemExt, sorted_iterator::AssumeSortedByItem,
};

pub trait BorrowMap<T, R: ?Sized> {
    fn borrow_map(x: &T) -> &R;
}
/// See [`std::convert::identity`]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Identity;
impl<T> BorrowMap<T, T> for Identity {
    fn borrow_map(x: &T) -> &T {
        x
    }
}

pub struct SortedSmallSet<
    Arr: Array,
    K: Ord + ?Sized = <Arr as Array>::Item,
    GetKey: BorrowMap<<Arr as Array>::Item, K> = Identity,
>(SmallVec<Arr>, PhantomData<(GetKey, K)>);

macro_rules! impl_sorted_small_set {
    ($(@$lifetime:lifetime [$($reference:tt)*])? $($trait:ty)? $(where [$($where_clauses:tt)+])? { $($implementation:tt)* }) => {
        impl<$($lifetime,)? Arr: Array, K: Ord + ?Sized, GetKey: BorrowMap<<Arr as Array>::Item, K>>
            $($trait for)? $($($reference)*)? SortedSmallSet<Arr, K, GetKey>
            $(where $($where_clauses)+)?
        {
            $($implementation)*
        }
    };
    (into_iter@ $($lifetime:lifetime $($mut:ident)?)? @ $iter:ty) => {
        impl_sorted_small_set!($(@$lifetime [&$lifetime$($mut)?])? IntoIterator {
            type Item = $(&$lifetime $($mut)?)? Arr::Item;

            type IntoIter = AssumeSortedByItem<$iter>;

            fn into_iter(self) -> Self::IntoIter {
                ($(&$($mut)?)? self.0).into_iter().assume_sorted_by_item()
            }
        });
    };
}

impl_sorted_small_set!(PartialEq {
    fn eq(&self, other: &Self) -> bool {
        Iterator::eq(
            self.into_iter().map(GetKey::borrow_map),
            other.into_iter().map(GetKey::borrow_map),
        )
    }
});
impl_sorted_small_set!(Ord {
    fn cmp(&self, other: &Self) -> Ordering {
        Iterator::cmp(
            self.into_iter().map(GetKey::borrow_map),
            other.into_iter().map(GetKey::borrow_map),
        )
    }
});
impl_sorted_small_set!(Eq {});
impl_sorted_small_set!(PartialOrd {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
});
impl_sorted_small_set!(Clone where [Arr::Item: Clone] {
    fn clone(&self) -> Self {
        Self(self.0.clone(), PhantomData)
    }
});
impl_sorted_small_set!(fmt::Debug where [Arr::Item: fmt::Debug] {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(&self.0).finish()
    }
});
impl_sorted_small_set!(Deref {
    // Deref but no DerefMut, since that could change the order
    // Technically, Deref doesn't prevent mutation (cells, atomics, mutexes, rw_lock, etc), but idgaf
    type Target = SmallVec<Arr>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
});
impl_sorted_small_set!(FromIterator<Arr::Item> {
    fn from_iter<T: IntoIterator<Item = Arr::Item>>(iter: T) -> Self {
        let mut res = iter.into_iter().collect::<SmallVec<_>>();
        res.sort_unstable_by(|a, b| Ord::cmp(GetKey::borrow_map(a), GetKey::borrow_map(b)));
        res.dedup_by(|a, b| GetKey::borrow_map(a) == GetKey::borrow_map(b));
        Self(res, PhantomData)
    }
});
impl_sorted_small_set!(into_iter@ @ smallvec::IntoIter<Arr>);
impl_sorted_small_set!(into_iter@ 'a @ std::slice::Iter<'a, Arr::Item>);
impl_sorted_small_set!(into_iter@ 'a mut @ std::slice::IterMut<'a, Arr::Item>);

impl_sorted_small_set!({
    /// Deref but no DerefMut, since that could change the order
    /// Technically, Deref doesn't prevent mutation (cells, atomics, mutexes, rw_lock, etc), but idgaf
    pub fn get(&self, item: &K) -> Option<&Arr::Item> {
        self.binary_search_by_key(&item, GetKey::borrow_map)
            .ok()
            .map(|idx| unsafe { self.get_unchecked(idx) })
    }

    pub fn empty() -> Self {
        Self(SmallVec::new(), PhantomData)
    }
    pub fn single(item: Arr::Item) -> Self {
        Self(smallvec![item], PhantomData)
    }

    /// SAFETY: both [`SortedIterator`] are correctly implemented
    pub unsafe fn union_iters(
        a: impl IntoIterator<IntoIter: SortedIterator<Item = Arr::Item>>,
        b: impl IntoIterator<IntoIter: SortedIterator<Item = Arr::Item>>,
    ) -> Self {
        Self(
            Union::<_, _, K, GetKey> {
                a: a.into_iter().peekable(),
                b: b.into_iter().peekable(),
                _phantom: PhantomData,
            }
            .collect(),
            PhantomData,
        )
    }
    pub fn union(a: Self, b: Self) -> Self {
        let (Self(mut smaller, _), Self(mut bigger, _)) =
            if a.len() < b.len() { (a, b) } else { (b, a) };

        let cap = bigger.len() + smaller.len();
        bigger.grow(cap);

        let mut out_idx = cap;
        while let Some(smaller_last) = smaller.last() {
            let write = if let Some(bigger_last) = bigger.last()
                && GetKey::borrow_map(bigger_last) > GetKey::borrow_map(smaller_last)
            {
                unsafe { bigger.pop().unwrap_unchecked() }
            } else {
                if bigger.last().map(GetKey::borrow_map) == Some(GetKey::borrow_map(smaller_last)) {
                    bigger.pop();
                }
                unsafe { smaller.pop().unwrap_unchecked() }
            };
            out_idx -= 1;
            unsafe { bigger.as_mut_ptr().add(out_idx).write(write) };
        }
        let len = cap - out_idx;
        {
            let remaining_bigger = bigger.len();
            if out_idx != remaining_bigger {
                unsafe {
                    bigger.set_len(0);
                    core::ptr::copy(
                        bigger.as_mut_ptr().add(out_idx),
                        bigger.as_mut_ptr().add(remaining_bigger),
                        len - remaining_bigger,
                    );
                }
            }
        }
        unsafe { bigger.set_len(len) };

        Self(bigger, PhantomData)
    }

    /// SAFETY: both [`SortedIterator`] are correctly implemented
    pub unsafe fn intersection_iters(
        a: impl IntoIterator<IntoIter: SortedIterator<Item = Arr::Item>>,
        b: impl IntoIterator<IntoIter: SortedIterator<Item = Arr::Item>>,
    ) -> Self {
        Self(
            Intersection::<_, _, K, GetKey> {
                a: a.into_iter(),
                b: b.into_iter().peekable(),
                _phantom: PhantomData,
            }
            .collect(),
            PhantomData,
        )
    }
    pub fn intersection(a: Self, b: Self) -> Self {
        let (Self(mut smaller, _), Self(mut bigger, _)) =
            if a.len() < b.len() { (a, b) } else { (b, a) };

        let cap = smaller.len();
        let mut out_idx = cap;
        while let Some(smaller_last) = smaller.pop() {
            let bigger_last = unsafe { bigger.pop().unwrap_unchecked() };
            out_idx -= 1;
            if GetKey::borrow_map(&smaller_last) == GetKey::borrow_map(&bigger_last) {
                unsafe { smaller.as_mut_ptr().add(out_idx).write(smaller_last) };
            }
        }

        let len = cap - out_idx;
        if out_idx != 0 {
            unsafe {
                core::ptr::copy(smaller.as_mut_ptr().add(out_idx), smaller.as_mut_ptr(), len)
            };
        }
        unsafe { smaller.set_len(len) };

        Self(smaller, PhantomData)
    }
});

/// Like [`sorted_iter::Union`], but with GetKey
struct Union<I: Iterator, J: Iterator, K: Ord + ?Sized, GetKey> {
    a: Peekable<I>,
    b: Peekable<J>,
    _phantom: PhantomData<(GetKey, K)>,
}
/// Like [`sorted_iter::Intersection`], but with GetKey
struct Intersection<I: Iterator, J: Iterator, K: Ord + ?Sized, GetKey> {
    a: I,
    b: Peekable<J>,
    _phantom: PhantomData<(GetKey, K)>,
}

impl<T, K: Ord + ?Sized, GetKey: BorrowMap<T, K>, I: Iterator<Item = T>, J: Iterator<Item = T>>
    Iterator for Union<I, J, K, GetKey>
{
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        if let (Some(ak), Some(bk)) = (self.a.peek(), self.b.peek()) {
            match Ord::cmp(GetKey::borrow_map(ak), GetKey::borrow_map(bk)) {
                Ordering::Less => self.a.next(),
                Ordering::Greater => self.b.next(),
                Ordering::Equal => {
                    self.b.next();
                    self.a.next()
                }
            }
        } else {
            self.a.next().or_else(|| self.b.next())
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let (amin, amax) = self.a.size_hint();
        let (bmin, bmax) = self.b.size_hint();
        // full overlap
        let rmin = Ord::max(amin, bmin);
        // no overlap
        let rmax = amax.and_then(|amax| bmax.and_then(|bmax| amax.checked_add(bmax)));
        (rmin, rmax)
    }
}

impl<T, K: Ord + ?Sized, GetKey: BorrowMap<T, K>, I: Iterator<Item = T>, J: Iterator<Item = T>>
    Iterator for Intersection<I, J, K, GetKey>
{
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(a) = self.a.next() {
            while let Some(b) = self.b.peek() {
                let order = Ord::cmp(GetKey::borrow_map(&a), GetKey::borrow_map(b));
                if order == Ordering::Less {
                    break;
                }
                self.b.next();
                if order == Ordering::Equal {
                    return Some(a);
                }
            }
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let (_, amax) = self.a.size_hint();
        let (_, bmax) = self.b.size_hint();
        // no overlap
        let rmin = 0;
        // full overlap
        let rmax = amax.and_then(|amax| bmax.map(|bmax| Ord::min(amax, bmax)));
        (rmin, rmax)
    }
}
