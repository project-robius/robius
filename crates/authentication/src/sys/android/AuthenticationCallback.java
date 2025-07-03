/* This file is compiled by build.rs. */

package robius.authentication;

import android.hardware.biometrics.BiometricPrompt;

public class AuthenticationCallback extends BiometricPrompt.AuthenticationCallback {
  private long pointer;

  /* TODO: There are neater ways of doing this */
  private native void rustCallback(long pointer, int errorCode, int helpCode);

  public AuthenticationCallback(long pointer) {
    this.pointer = pointer;
  }

  public void onAuthenticationError(int errorCode, CharSequence errString) {
    rustCallback(pointer, errorCode, 0);
  }

  /* This is called when the user presents an incorrect authenticator (e.g. fingerprint or password). However, the
   * prompt remains displayed and so we don't really care, until the prompt dissapears, in which case
   * `onAuthenticationError`, `onAuthenticationHelp`, or `onAuthenticationSucceeded` is called. */
  public void onAuthenticationFailed() {}

  public void onAuthenticationHelp(int helpCode, CharSequence helpString) {
    rustCallback(pointer, 0, helpCode);
  }

  public void onAuthenticationSucceeded(BiometricPrompt.AuthenticationResult result) {
    rustCallback(pointer, 0, 0);
  }
}