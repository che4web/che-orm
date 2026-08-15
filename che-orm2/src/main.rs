mod models;
fn main() {
    let u = models::User::new("test".into());
    let b = models::User::NAME;
    println!("Hello, world! {u:?} {:?}", b);
}
