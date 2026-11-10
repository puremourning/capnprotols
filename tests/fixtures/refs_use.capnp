@0xc3a1f0e2b4d5a602;

using Lib = import "refs_lib.capnp";

struct Canvas {
  centre @0 :Lib.Point;
  shapes @1 :List(Lib.Shape);
  styles @2 :List(Lib.Shape.Style);
}
