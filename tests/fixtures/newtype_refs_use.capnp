@0xe227b60577350202;

using Lib = import "newtype_refs_lib.capnp";

struct Transfer {
  from @0 :Lib.AccountId;
  amount @[1,2] :Lib.Money;
  to @[3,4] :Lib.Payee;
  cc @5 :List(Lib.AccountId);
}
