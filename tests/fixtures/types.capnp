@0xb294b46674a10e44;

using UUID = Data;
using UTCSecondsSinceEpoch = UInt64;

enum Side {
  buy @0;
  sell @1;
}

struct Date {
  year @0 :UInt16;
  month @1 :UInt8;
  day @2 :UInt8;
}
