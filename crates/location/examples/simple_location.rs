use robius_location::{Access, Accuracy, Error, Location, Manager};

struct Handler;

impl robius_location::Handler for Handler {
    fn handle(&self, location: Location<'_>) {
        println!(
            "received {:?} location: coordinates={:?}, altitude={:?}, bearing={:?}, speed={:?}, \
             time={:?}",
            location.freshness(),
            location.coordinates(),
            location.altitude(),
            location.bearing(),
            location.speed(),
            location.time(),
        );
    }

    fn error(&self, e: Error) {
        println!("received error: {e:?}");
    }
}

fn main() {
    let mut manager = Manager::new(Handler).expect("location service is unavailable");

    if let Err(error) = manager.request_authorization(Access::Foreground, Accuracy::Precise) {
        eprintln!("could not authorize location access: {error:?}");
        return;
    }
    if let Err(error) = manager.start_updates() {
        eprintln!("could not start location updates: {error:?}");
        return;
    }

    loop {
        std::thread::park();
    }
}
