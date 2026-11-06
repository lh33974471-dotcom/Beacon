import 'package:flutter/services.dart';

class SoftKeyboardEdit {
  final int backspaces;
  final String inserted;
  const SoftKeyboardEdit(this.backspaces, this.inserted);
}

const int kMaxSoftKeyboardBackspaces = 32;

SoftKeyboardEdit computeSoftKeyboardEdit(String oldValue, String newValue) {
  var prefix = 0;
  while (prefix < oldValue.length &&
      prefix < newValue.length &&
      oldValue.codeUnitAt(prefix) == newValue.codeUnitAt(prefix)) {
    prefix++;
  }
  var suffix = 0;
  while (suffix < oldValue.length - prefix &&
      suffix < newValue.length - prefix &&
      oldValue.codeUnitAt(oldValue.length - 1 - suffix) ==
          newValue.codeUnitAt(newValue.length - 1 - suffix)) {
    suffix++;
  }
  return SoftKeyboardEdit(
    oldValue.length - prefix - suffix,
    newValue.substring(prefix, newValue.length - suffix),
  );
}

String committedText(TextEditingValue value) => value.isComposingRangeValid
    ? value.composing.textBefore(value.text) +
        value.composing.textAfter(value.text)
    : value.text;
