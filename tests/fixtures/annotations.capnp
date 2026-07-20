@0xc9214ea7eb78b970;

enum EnumThing {
  one @0;
  two @1;
}

struct StructThing {
  thing @0: EnumThing;
  thong @1: Text;
  thang @2: Int16;
}

annotation listof(struct) :List(EnumThing);
annotation sructof(struct) :StructThing;

struct ListOf $listof([one, two]) $sructof(thing=one, thong="text", thang=1) {
    foo @0 :Text;
}
