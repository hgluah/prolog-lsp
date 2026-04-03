use core::{cmp::Ordering, fmt, marker::PhantomData};
use std::{iter::Peekable, mem::MaybeUninit, ops::RangeBounds};

use smallvec::{Array, SmallVec};
use sorted_iter::{
    SortedIterator, assume::AssumeSortedByItemExt, sorted_iterator::AssumeSortedByItem,
};

pub trait SSSHandler<T> {
    type Key: ?Sized + Ord;
    fn key(x: &T) -> &Self::Key;
    fn reduce(old: &mut T, new: T) {
        *old = new;
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Noop;
impl<T: Ord> SSSHandler<T> for Noop {
    type Key = T;

    /// See [`std::convert::identity`]
    fn key(x: &T) -> &T {
        x
    }
}

pub struct SSSEntry<'a, Arr: Array, Handler: SSSHandler<Arr::Item>>(
    &'a mut SmallVec<Arr>,
    Result<usize, usize>,
    PhantomData<Handler>,
);
impl<'a, Arr: Array, Handler: SSSHandler<Arr::Item>> SSSEntry<'a, Arr, Handler> {
    /// SAFETY: The caller cannot modify data that modifies the order of the elements
    #[inline]
    pub unsafe fn get(self) -> Option<&'a mut Arr::Item> {
        self.1
            .ok()
            .map(|idx| unsafe { self.0.get_unchecked_mut(idx) })
    }

    /// SAFETY: The inserted (and/or reduced) item must have a key that doesn't modify the order of the elements
    #[inline]
    pub unsafe fn insert(self, x: Arr::Item) -> &'a mut Arr::Item {
        match self.1 {
            Ok(idx) => {
                let res = unsafe { self.0.get_unchecked_mut(idx) };
                Handler::reduce(res, x);
                #[cfg(debug_assertions)]
                {
                    let res = unsafe { self.0.get_unchecked(idx) };
                    debug_assert!(
                        idx.checked_sub(1)
                            .and_then(|idx| self.0.get(idx))
                            .map(|before| Handler::key(before) < Handler::key(res))
                            != Some(false)
                    );
                    debug_assert!(
                        idx.checked_add(1)
                            .and_then(|idx| self.0.get(idx))
                            .map(|after| Handler::key(after) > Handler::key(res))
                            != Some(false)
                    );
                    unsafe { self.0.get_unchecked_mut(idx) }
                }
                #[cfg(not(debug_assertions))]
                res
            }
            Err(idx) => {
                self.0.insert(idx, x);
                // self.1 = Ok(idx);
                unsafe { self.0.get_unchecked_mut(idx) }
            }
        }
    }

    /// SAFETY: The inserted item must have a key that doesn't modify the order of the elements
    #[inline]
    pub unsafe fn get_or_insert(self, x: impl FnOnce() -> Arr::Item) -> &'a mut Arr::Item {
        let idx = match self.1 {
            Ok(idx) => idx,
            Err(idx) => {
                self.0.insert(idx, x());
                // self.1 = Ok(idx);
                idx
            }
        };
        unsafe { self.0.get_unchecked_mut(idx) }
    }

    /// SAFETY: The default item must have a key that doesn't modify the order of the elements
    #[inline]
    pub unsafe fn get_or_default(self) -> &'a mut Arr::Item
    where
        Arr::Item: Default,
    {
        unsafe { self.get_or_insert(Default::default) }
    }
}

pub struct SortedSmallSet<T, const N: usize, Handler: SSSHandler<T> = Noop>(
    SmallVec<[T; N]>,
    PhantomData<Handler>,
);

macro_rules! impl_sorted_small_set {
    ($(@$lifetime:lifetime [$($reference:tt)*])? $($trait:ty)? $(where [$($where_clauses:tt)+])? { $($implementation:tt)* }) => {
        impl<$($lifetime,)? T, const N: usize, Handler: SSSHandler<T>>
        $($trait for)? $($($reference)*)? SortedSmallSet<T, N, Handler>
            $(where $($where_clauses)+)?
        {
            $($implementation)*
        }
    };
    (into_iter@ $($lifetime:lifetime $($mut:ident)?)? @ $iter:ty) => {
        impl_sorted_small_set!($(@$lifetime [&$lifetime$($mut)?])? IntoIterator {
            type Item = $(&$lifetime $($mut)?)? T;

            type IntoIter = AssumeSortedByItem<$iter>;

            fn into_iter(self) -> Self::IntoIter {
                ($(&$($mut)?)? self.0).into_iter().assume_sorted_by_item()
            }
        });
    };
}

impl_sorted_small_set!(Default {
    fn default() -> Self {
        Self::empty()
    }
});
impl_sorted_small_set!(PartialEq {
    fn eq(&self, other: &Self) -> bool {
        Iterator::eq(
            self.into_iter().map(Handler::key),
            other.into_iter().map(Handler::key),
        )
    }
});
impl_sorted_small_set!(Ord {
    fn cmp(&self, other: &Self) -> Ordering {
        Iterator::cmp(
            self.into_iter().map(Handler::key),
            other.into_iter().map(Handler::key),
        )
    }
});
impl_sorted_small_set!(Eq {});
impl_sorted_small_set!(PartialOrd {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
});
impl_sorted_small_set!(Clone where [T: Clone] {
    fn clone(&self) -> Self {
        Self(self.0.clone(), PhantomData)
    }
});
impl_sorted_small_set!(fmt::Debug where [T: fmt::Debug] {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(&self.0).finish()
    }
});
impl_sorted_small_set!(FromIterator<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut res = iter.into_iter().collect::<SmallVec<_>>();
        res.sort_unstable_by(|a, b| Ord::cmp(Handler::key(a), Handler::key(b)));
        res.dedup_by(|a, b| Handler::key(a) == Handler::key(b));
        Self(res, PhantomData)
    }
});
impl_sorted_small_set!(into_iter@ @ smallvec::IntoIter<[T; N]>);
impl_sorted_small_set!(into_iter@ 'a @ std::slice::Iter<'a, T>);
impl_sorted_small_set!(into_iter@ 'a mut @ std::slice::IterMut<'a, T>);

impl_sorted_small_set!({
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    #[inline]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// SAFETY: The caller cannot modify data that modifies the order of the elements
    #[inline]
    pub unsafe fn get(&self, item: &Handler::Key) -> Option<&T> {
        self.0
            .binary_search_by_key(&item, Handler::key)
            .ok()
            .map(|idx| unsafe { self.0.get_unchecked(idx) })
    }
    /// SAFETY: The caller cannot modify data that modifies the order of the elements
    #[inline]
    pub unsafe fn get_mut(&mut self, item: &Handler::Key) -> Option<&mut T> {
        unsafe { self.entry(item).get() }
    }
    #[inline]
    pub fn entry(&mut self, item: &Handler::Key) -> SSSEntry<[T; N], Handler> {
        let idx = self.0.binary_search_by_key(&item, Handler::key);
        SSSEntry(&mut self.0, idx, PhantomData)
    }

    #[inline]
    pub fn drain(&mut self, range: impl RangeBounds<usize>) -> smallvec::Drain<'_, [T; N]> {
        self.0.drain(range)
    }

    pub const fn empty() -> Self {
        Self(SmallVec::new_const(), PhantomData)
    }
    pub fn single(item: T) -> Self {
        const { assert!(N > 0) };
        let mut arr = const { MaybeUninit::uninit().transpose() };
        arr[0] = MaybeUninit::new(item);
        Self(
            unsafe { SmallVec::from_buf_and_len_unchecked(arr.transpose(), 1) },
            PhantomData,
        )
    }

    /// SAFETY: both [`SortedIterator`] are correctly implemented
    pub unsafe fn union_iters(
        a: impl IntoIterator<IntoIter: SortedIterator<Item = T>>,
        b: impl IntoIterator<IntoIter: SortedIterator<Item = T>>,
    ) -> Self {
        Self(
            Union::<_, _, Handler> {
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
                && Handler::key(bigger_last) > Handler::key(smaller_last)
            {
                unsafe { bigger.pop().unwrap_unchecked() }
            } else {
                let mut smaller_last = unsafe { smaller.pop().unwrap_unchecked() };
                if bigger.last().map(Handler::key) == Some(Handler::key(&smaller_last)) {
                    Handler::reduce(&mut smaller_last, unsafe {
                        bigger.pop().unwrap_unchecked()
                    });
                }
                smaller_last
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
        a: impl IntoIterator<IntoIter: SortedIterator<Item = T>>,
        b: impl IntoIterator<IntoIter: SortedIterator<Item = T>>,
    ) -> Self {
        Self(
            Intersection::<_, _, Handler> {
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
        while let Some(mut smaller_last) = smaller.pop() {
            let bigger_last = unsafe { bigger.pop().unwrap_unchecked() };
            out_idx -= 1;
            if Handler::key(&smaller_last) == Handler::key(&bigger_last) {
                Handler::reduce(&mut smaller_last, bigger_last);
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

/// Like [`sorted_iter::Union`], but with Handler
struct Union<I: Iterator, J: Iterator, Handler> {
    a: Peekable<I>,
    b: Peekable<J>,
    _phantom: PhantomData<Handler>,
}
/// Like [`sorted_iter::Intersection`], but with Handler
struct Intersection<I: Iterator, J: Iterator, Handler> {
    a: I,
    b: Peekable<J>,
    _phantom: PhantomData<Handler>,
}

impl<T, Handler: SSSHandler<T>, I: Iterator<Item = T>, J: Iterator<Item = T>> SortedIterator
    for Union<I, J, Handler>
{
}
impl<T, Handler: SSSHandler<T>, I: Iterator<Item = T>, J: Iterator<Item = T>> SortedIterator
    for Intersection<I, J, Handler>
{
}
impl<T, Handler: SSSHandler<T>, I: Iterator<Item = T>, J: Iterator<Item = T>> Iterator
    for Union<I, J, Handler>
{
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        if let (Some(ak), Some(bk)) = (self.a.peek(), self.b.peek()) {
            match Ord::cmp(Handler::key(ak), Handler::key(bk)) {
                Ordering::Less => self.a.next(),
                Ordering::Greater => self.b.next(),
                Ordering::Equal => {
                    let mut res = unsafe { self.a.next().unwrap_unchecked() };
                    Handler::reduce(&mut res, unsafe { self.b.next().unwrap_unchecked() });
                    Some(res)
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

impl<T, Handler: SSSHandler<T>, I: Iterator<Item = T>, J: Iterator<Item = T>> Iterator
    for Intersection<I, J, Handler>
{
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(mut a) = self.a.next() {
            while let Some(b) = self.b.peek() {
                let order = Ord::cmp(Handler::key(&a), Handler::key(b));
                if order == Ordering::Less {
                    break;
                }
                let b = unsafe { self.b.next().unwrap_unchecked() };
                if order == Ordering::Equal {
                    Handler::reduce(&mut a, b);
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
