@0x84209b158e994434;

using Types = import "types.capnp";
using Json = import "/capnp/compat/json.capnp";

annotation pii(field) :Void;

struct Organisation {
  # An organisation owning resources.
  organisationId @0 :Types.UUID $Json.hex;
  code @1 :Text;
}

struct AuthToken {
  # Opaque session token used in subsequent requests.
  token @0 :Text;
  expiresAt @1 :Types.UTCSecondsSinceEpoch;
}

struct CertificateBundle {
  certs @0 :List(AuthToken);
}
