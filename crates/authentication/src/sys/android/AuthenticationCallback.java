/* This file is compiled by build.rs. */

package robius.authentication;

import android.content.DialogInterface;
import android.hardware.biometrics.BiometricPrompt;

public class AuthenticationCallback extends BiometricPrompt.AuthenticationCallback
    implements DialogInterface.OnClickListener {
  private long pointer;

  private native void rustCallback(long pointer, int errorCode);

  public AuthenticationCallback(long pointer) {
    this.pointer = pointer;
  }

  /* Invokes the Rust callback at most once; the Rust side frees `pointer` when invoked,
   * so calling it a second time would be a use-after-free. */
  private synchronized void invokeOnce(int errorCode) {
    if (pointer != 0) {
      rustCallback(pointer, errorCode);
      pointer = 0;
    }
  }

  public void onAuthenticationError(int errorCode, CharSequence errString) {
    invokeOnce(errorCode);
  }

  /* Called when the user presents an invalid credential (e.g., an unrecognized fingerprint).
   * Non-terminal: the prompt remains displayed and the user may retry. */
  public void onAuthenticationFailed() {}

  /* Called with transient feedback during authentication (e.g., "Sensor dirty").
   * Non-terminal: the prompt remains displayed, so this must not consume the Rust callback. */
  public void onAuthenticationHelp(int helpCode, CharSequence helpString) {}

  public void onAuthenticationSucceeded(BiometricPrompt.AuthenticationResult result) {
    invokeOnce(0);
  }

  /* Handles a click on the prompt's negative button (set when device credential
   * fallback is not allowed). */
  public void onClick(DialogInterface dialog, int which) {
    invokeOnce(BiometricPrompt.BIOMETRIC_ERROR_USER_CANCELED);
  }
}
