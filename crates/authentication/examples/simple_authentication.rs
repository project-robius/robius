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
