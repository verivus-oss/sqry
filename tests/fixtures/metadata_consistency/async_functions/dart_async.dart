// Dart async function test fixture

Future<String> fetchData() async {
  await Future.delayed(Duration(seconds: 1));
  return "data";
}

String syncFunction() {
  return "sync";
}
