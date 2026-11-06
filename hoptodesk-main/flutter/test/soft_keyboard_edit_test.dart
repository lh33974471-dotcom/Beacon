import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flutter_hbb/mobile/pages/soft_keyboard_edit.dart';

void main() {
  test('typing a character at the end sends just that character', () {
    final e = computeSoftKeyboardEdit('1111qwer', '1111qwert');
    expect(e.backspaces, 0);
    expect(e.inserted, 't');
  });

  test('deleting the last character sends one backspace and no text', () {
    final e = computeSoftKeyboardEdit('1111qwert', '1111qwer');
    expect(e.backspaces, 1);
    expect(e.inserted, '');
  });

  test('an insert at the start is not read as the trailing character', () {
    // The reported bug: with the caret at the front the field grows by one, and
    // taking newValue.substring(oldValue.length) returns the last character of
    // the buffer every time, so every keystroke repeated 't'.
    final e = computeSoftKeyboardEdit('1111qwert', 'h1111qwert');
    expect(e.inserted, 'h');
    expect(e.backspaces, 0);
    expect('h1111qwert'.substring('1111qwert'.length), 't');
  });

  test('an insert in the middle reports the inserted character', () {
    final e = computeSoftKeyboardEdit('1abc', '1axbc');
    expect(e.backspaces, 0);
    expect(e.inserted, 'x');
  });

  test('a replacement reports both the deletion and the insertion', () {
    final e = computeSoftKeyboardEdit('1abc', '1axc');
    expect(e.backspaces, 1);
    expect(e.inserted, 'x');
  });

  test('an unchanged value produces no input at all', () {
    final e = computeSoftKeyboardEdit('1111', '1111');
    expect(e.backspaces, 0);
    expect(e.inserted, '');
  });

  test('a multi-character paste is reported whole', () {
    final e = computeSoftKeyboardEdit('1111', '1111hello');
    expect(e.backspaces, 0);
    expect(e.inserted, 'hello');
  });

  test('deleting several characters reports each one', () {
    final e = computeSoftKeyboardEdit('1111qwert', '1111q');
    expect(e.backspaces, 4);
    expect(e.inserted, '');
  });

  test('repeated identical characters do not confuse the suffix scan', () {
    final e = computeSoftKeyboardEdit('1111', '11111');
    expect(e.backspaces, 0);
    expect(e.inserted, '1');
  });

  test('text still being composed is not committed', () {
    const v = TextEditingValue(
        text: '1111中国', composing: TextRange(start: 4, end: 6));
    expect(committedText(v), '1111');
  });

  test('ending composition commits the text even when it did not change', () {
    const before = TextEditingValue(
        text: '1111中国', composing: TextRange(start: 4, end: 6));
    const after = TextEditingValue(text: '1111中国');
    expect(before.text, after.text);
    expect(committedText(after), '1111中国');
  });

  test('a committed character stays sent while the next one is composed', () {
    const v = TextEditingValue(
        text: '1111中国', composing: TextRange(start: 5, end: 6));
    expect(committedText(v), '1111中');
  });
}
