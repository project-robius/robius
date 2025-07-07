use robius_location::{Access, Accuracy, Error, Location, Manager};

struct Handler;

impl robius_location::Handler for Handler {
    fn handle(&self, location: Location<'_>) {
        println!("received location: {:?}", location.time());
    }

    fn error(&self, e: Error) {
        println!("received error: {e:?}");
    }
}

fn main() {
    let mut manager = Manager::new(Handler).unwrap();

    manager
        .request_authorization(Access::Foreground, Accuracy::Precise)
        .unwrap();
    manager.start_updates().unwrap();

    loop {}
}
