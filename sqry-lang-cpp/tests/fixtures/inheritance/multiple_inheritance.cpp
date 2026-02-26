// Multiple inheritance test
class InterfaceA {
public:
    virtual void methodA() = 0;
    virtual ~InterfaceA() = default;
};

class InterfaceB {
public:
    virtual void methodB() = 0;
    virtual ~InterfaceB() = default;
};

class Implementation : public InterfaceA, public InterfaceB {
public:
    void methodA() override {}
    void methodB() override {}
};
