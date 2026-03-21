// Template class inheritance test
template <typename T>
class Container {
public:
    virtual void add(T item) = 0;
    virtual T get(int index) = 0;
    virtual ~Container() = default;
};

template <typename T>
class ArrayContainer : public Container<T> {
public:
    void add(T item) override {}
    T get(int index) override { return T{}; }
private:
    T* data;
};

// Non-template class inheriting from template
class IntList : public Container<int> {
public:
    void add(int item) override {}
    int get(int index) override { return 0; }
};
