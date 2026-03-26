package main

type Reader interface {
	Read(p []byte) (n int, err error)
}

type Writer interface {
	Write(p []byte) (n int, err error)
}

type FileHandler struct{}

func (f *FileHandler) Read(p []byte) (int, error) {
	return 0, nil
}

func (f *FileHandler) Write(p []byte) (int, error) {
	return len(p), nil
}

func processData(r Reader, w Writer) {
	data := make([]byte, 1024)
	n, _ := r.Read(data)
	w.Write(data[:n])
}
