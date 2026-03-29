namespace outer {
namespace middle {
namespace inner {
namespace deep {

class MyClass {
public:
    void method();

    class NestedClass {
        void nestedMethod();
    };
};

void MyClass::method() {
    // Qualified method implementation
}

} // namespace deep
} // namespace inner
} // namespace middle
} // namespace outer

using namespace outer::middle;
using outer::middle::inner::deep::MyClass;
