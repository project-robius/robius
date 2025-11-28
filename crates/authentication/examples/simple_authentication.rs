use robius_authentication::{
    AndroidText, BiometricStrength, Context, Policy, PolicyBuilder, Text, WindowsText,
};

const POLICY: Policy = PolicyBuilder::new()
    .biometrics(Some(BiometricStrength::Strong))
    .password(true)
    .companion(true)
    .build()
    .unwrap();

const TEXT: Text = Text {
    android: AndroidText {
        title: "Title",
        subtitle: None,
        description: None,
    },
    apple: "authenticate",
    windows: WindowsText::new("Title", "Description").unwrap(),
};

fn main() {
    let context = Context::new(());

    #[cfg(target_os = "linux")]
    {
        use std::sync::mpsc;
        use std::time::Duration;

        let (tx, rx) = mpsc::channel();

        let res = context.authenticate(TEXT, &POLICY, move |result| {
            let _ = tx.send(result);
        });

        if let Err(e) = res {
            eprintln!("Authentication request failed early: {:?}", e);
            return;
        }

        match rx.recv_timeout(Duration::from_secs(120)) {
            Ok(Ok(_)) => println!("Authentication successful"),
            Ok(Err(e)) => println!("Authentication failed: {:?}", e),
            Err(_) => println!("Authentication timed out / callback not fired"),
        }

        return;
    }

    #[cfg(not(target_os = "linux"))]
    {
        let res = context.authenticate(
            TEXT,
            &POLICY,
            |result| match result {
                Ok(_) => println!("Authentication successful"),
                Err(e) => println!("Authentication failed: {:?}", e),
            },
        );
        
        // Note: if `res` is `Ok`, the authentication did not necessarily succeed. 
        // The callback will be called with the result of the authentication.
        // If `res` is `Err`, it indicates an error in the authentication policy or context setup.
        if let Err(e) = res {
            eprintln!("Authentication failed: {:?}", e);
        }
    }
}
