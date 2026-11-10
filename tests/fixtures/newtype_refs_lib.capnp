@0xe227b60577350201;

type AccountId = UInt64;

type Money = group {
  units @0 :Int64;
  nanos @1 :Int32;
}

type Payee = union {
  account @0 :AccountId;
  email @1 :Text;
}

type Owner = AccountId;

struct Account {
  id @0 :AccountId;
  balance @[1,2] :Money;
  owner @3 :Owner;
}
