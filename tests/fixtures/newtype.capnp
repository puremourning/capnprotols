@0xe227b60577350102;

using Json = import "/capnp/compat/json.capnp";

using Foo = Data;

type UUID = Data $Json.hex;

type Price = group {
  value @0 :Int64;
  scale @1 :UInt16;
}

enum Side {
  buy @0;
  sell @1;
}

struct Order {
  id @0 :UUID;
  price @[1,3] :Price;
  quantity @2 :Int64;
  side @4 :Side;
  test @5 :Foo;
}

