Hand-written KiCad library fixtures for `tests/kicad_lib_import.rs`.

Written for cadlab from KiCad's published S-expression format documentation; they are not taken,
copied or converted from KiCad's libraries (DECISIONS D7). MIT, like the rest of cadlab.

- `cadlab_test.pretty/`: footprints in the KiCad 6 (`SOT23_TEST`, `CUSTOM_TEST`) and KiCad 8
  (`R0603_TEST`, `HDR_1x03_TEST`) formats.
- `cadlab_test.kicad_sym`: a KiCad 6 symbol library (regulator, derived symbol, dual op-amp
  with a De Morgan body style, pins with alternate functions, a power symbol, a resistor, a
  connector).
