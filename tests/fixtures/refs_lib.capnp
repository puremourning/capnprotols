@0xc3a1f0e2b4d5a601;

struct Point {
  x @0 :Int32;
  y @1 :Int32;
}

struct Shape {
  struct Style {
    colour @0 :Text;
  }

  origin @0 :Point;
  vertices @1 :List(Point);
  style @2 :Style;
}
