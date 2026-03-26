// Single inheritance test
class Base {
public:
    virtual void process() {}
    virtual ~Base() = default;
};

class Derived : public Base {
public:
    void process() override {}
};
