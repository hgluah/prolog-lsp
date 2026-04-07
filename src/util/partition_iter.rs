use std::collections::VecDeque;

use either::Either;

pub struct PartitionIter<I: Iterator, A, B, F: FnMut(I::Item) -> Either<A, B>> {
    pub iter: I,
    pub a: VecDeque<A>,
    pub b: VecDeque<B>,
    pub partitioner: F,
}
impl<I: Iterator, A, B, F: FnMut(I::Item) -> Either<A, B>> PartitionIter<I, A, B, F> {
    pub fn new(iter: impl IntoIterator<IntoIter = I>, partitioner: F) -> Self {
        Self {
            iter: iter.into_iter(),
            a: VecDeque::new(),
            b: VecDeque::new(),
            partitioner,
        }
    }
    pub fn next_a(&mut self) -> Option<A> {
        self.a.pop_front().or_else(|| {
            while let Some(x) = self.iter.next() {
                match (self.partitioner)(x) {
                    Either::Left(a) => return Some(a),
                    Either::Right(b) => self.b.push_back(b),
                }
            }
            None
        })
    }
    pub fn next_b(&mut self) -> Option<B> {
        self.b.pop_front().or_else(|| {
            while let Some(x) = self.iter.next() {
                match (self.partitioner)(x) {
                    Either::Left(a) => self.a.push_back(a),
                    Either::Right(b) => return Some(b),
                }
            }
            None
        })
    }
}
