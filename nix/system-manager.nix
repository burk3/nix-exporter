# Not tested beyond evaluation; see the README.
{ self }:
{
  imports = [ (import ./module.nix { inherit self; }) ];
}
