use che_orm2::DbEnum;

#[derive(DbEnum)]
enum Status<T> {
    Draft,
    _Marker(std::marker::PhantomData<T>),
}

fn main() {}
