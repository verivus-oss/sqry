// Struct inheritance test (public by default in C++)
struct BaseStruct {
    int value;
};

struct DerivedStruct : BaseStruct {
    int extra_value;
};

// Struct inheriting from class
class BaseClass {
public:
    virtual void process() {}
};

struct StructFromClass : public BaseClass {
    void process() override {}
};
