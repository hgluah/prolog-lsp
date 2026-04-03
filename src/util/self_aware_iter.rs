use std::ops::ControlFlow;

trait MyTry {
    type Output;
    type Residual;

    fn into_result(self) -> Result<Self::Output, Self::Residual>;
    fn from_output(x: Self::Output) -> Self;
    fn from_residual(x: Self::Residual) -> Self;
}

impl<T> MyTry for Option<T> {
    type Output = T;
    type Residual = ();

    fn into_result(self) -> Result<Self::Output, Self::Residual> {
        match self {
            Some(x) => Ok(x),
            None => Err(()),
        }
    }
    fn from_output(x: Self::Output) -> Self {
        Some(x)
    }
    fn from_residual((): Self::Residual) -> Self {
        None
    }
}
impl<T, E> MyTry for Result<T, E> {
    type Output = T;
    type Residual = E;

    fn into_result(self) -> Result<Self::Output, Self::Residual> {
        self
    }
    fn from_output(x: Self::Output) -> Self {
        Ok(x)
    }
    fn from_residual(x: Self::Residual) -> Self {
        Err(x)
    }
}
impl<T, E> MyTry for ControlFlow<T, E> {
    type Output = E;
    type Residual = T;

    fn into_result(self) -> Result<Self::Output, Self::Residual> {
        match self {
            ControlFlow::Continue(x) => Ok(x),
            ControlFlow::Break(x) => Err(x),
        }
    }
    fn from_output(x: Self::Output) -> Self {
        ControlFlow::Continue(x)
    }
    fn from_residual(x: Self::Residual) -> Self {
        ControlFlow::Break(x)
    }
}

pub trait SelfAwareIterator {
    type Item<'a>
    where
        Self: 'a;

    fn next<'s>(&'s mut self) -> Option<Self::Item<'s>>;

    fn try_fold<'s, B, R: MyTry<Output = B>>(
        &'s mut self,
        init: B,
        f: for<'a> fn(B, Self::Item<'a>) -> R,
    ) -> R {
        let mut accum = init;
        while let Some(x) = self.next() {
            accum = match f(accum, x).into_result() {
                Ok(new_accum) => new_accum,
                Err(residual) => return R::from_residual(residual),
            };
        }
        R::from_output(accum)
    }
}
