# Writes a Nix value as ZON, the configuration format of kwm and kwim.
#   - An attribute set is a struct. A list is a tuple.
#   - (enum "x") is the enum literal .x.
#   - (hex "x") is the integer literal 0xx.
{ lib }:
rec {
  enum = name: { _zon = ".${name}"; };
  hex = digits: { _zon = "0x${digits}"; };

  # "#29272b" gives 0x29272bff.
  rgba = color: hex "${lib.removePrefix "#" color}ff";

  ident =
    name:
    if builtins.match "[A-Za-z_][A-Za-z0-9_]*" name != null then name else "@${builtins.toJSON name}";

  toZon =
    indent: value:
    let
      next = indent + "    ";
      block = items: if items == [ ] then ".{}" else ".{\n${lib.concatStrings items}${indent}}";
    in
    if builtins.isAttrs value && value ? _zon then
      value._zon
    else if builtins.isAttrs value then
      block (lib.mapAttrsToList (name: v: "${next}.${ident name} = ${toZon next v},\n") value)
    else if builtins.isList value then
      block (map (v: "${next}${toZon next v},\n") value)
    else if builtins.isString value then
      builtins.toJSON value
    else if builtins.isBool value then
      lib.boolToString value
    else if value == null then
      "null"
    else if builtins.isFloat value then
      lib.strings.floatToString value
    else
      toString value;

  generate = value: toZon "" value + "\n";
}
