use robius_authentication::{
    AndroidText, BiometricStrength, Context, Policy, PolicyBuilder, Text, WindowsText,
};

/// If you want to run this simple demo on linux, please ensure policy file installed correctly 
/// and to setting your action id by .action_id(<YOUR_POLICY_SETTING_ACTION_ID>)
///s See: README file `Usage on Linux` section.
const POLICY: Policy = PolicyBuilder::new()
    .action_id("org.robius.authentication")
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
