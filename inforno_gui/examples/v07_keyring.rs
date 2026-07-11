/*
minimal program to check whether keyring is working on a target machine
[dependencies]
keyring = { version = "3.6.3", features = ["linux-native", "apple-native", "windows-native", "vendored"] }
secrecy = "0.10.3"
zeroize = "1.8.2"
*/

use keyring::Entry;
use secrecy::{ExposeSecret, SecretString, zeroize};
use zeroize::Zeroizing;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Define service and username for the keyring entry
    let service = "my_example_app";
    let username = "user@example.com";

    // Create a keyring entry
    let entry = Entry::new(service, username)?;

    // Store a password
    let password = "super_secret_password123";
    entry.set_password(password)?;
    println!("Password stored successfully!");

    // Retrieve password and wrap in SecretString for secure handling
    let retrieved = Zeroizing::new(entry.get_password()?);
    let secret_password: SecretString = retrieved.as_str().into();

    // Access the secret value only when needed
    println!("Retrieved password: {}", secret_password.expose_secret());

    // Delete the password when done (optional)
    entry.delete_credential()?;
    println!("Password deleted!");

    Ok(())
}