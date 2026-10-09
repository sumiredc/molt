// expect: phantom_data, generic_param
use std::marker::PhantomData;

struct Tagged<T> {
    tag: PhantomData<T>,
}
