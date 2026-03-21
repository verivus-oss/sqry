import re
import difflib
from rich import print 
from rich.markup import escape
from rich.prompt import Confirm
from sys import argv

import sexpparse
from tree_sitter import Language, Parser


Language.build_library(
  'build/languages.so',
  ['.']
)
GROOVY_LANGUAGE = Language('build/languages.so', 'groovy')

class TestFileParseError(Exception):
    pass

def parse_test_file_lines(lines):
    state = 'start'
    tests = {}
    test_name = None
    code = []
    sexpr = []
    for l in lines:
        l = l.rstrip()
        # print(l, state)
        if not l:
            continue
        if state == 'start':
            if re.fullmatch(r'=+', l):
                state = 'test_name'
        elif state == 'test_name':
            if re.fullmatch(r'=+', l):
                raise TestFileParseError('no test name')
            test_name = l
            state = 'wait_end_test_name'
        elif state == 'wait_end_test_name':
            if re.fullmatch(r'=+', l):
                state = 'code'
            else:
                raise TestFileParseError('multi-line test name')
        elif state == 'code':
            if re.fullmatch(r'---+', l):
                state = 'sexpr'
            else:
                code.append(l)
        elif state == 'sexpr':
            if re.fullmatch(r'===+', l):
                tests[test_name] = [
                    bytes('\n'.join(code), 'utf-8'),
                    '\n'.join(sexpr),
                ]
                test_name = None
                code = []
                sexpr = []
                state = 'test_name'
            else:
                sexpr.append(l)
    if test_name:
        tests[test_name] = [
            bytes('\n'.join(code), 'utf-8'),
            '\n'.join(sexpr),
        ]
    return tests

def print_diff(s1, s2):
    ndiff = difflib.ndiff(
        s1.split('\n'),
        s2.split('\n'),
    )
    for line in ndiff:
        if line.startswith('+'):
            print(f'[on color(22)]{line}[/]')
        elif line.startswith('-'):
            print(f'[on color(52)]{line}[/]')
        elif not line or line.startswith('?'):
            continue
        else:
            print(line)

filename = argv[1]
tests = parse_test_file_lines(open(filename).readlines())
groovy_parser = Parser()
groovy_parser.set_language(GROOVY_LANGUAGE)
unupdated_failures = 0
updates = 0
for test_name, (code, expected_sexp) in tests.items():
    tree = groovy_parser.parse(code)
    # print(tree.root_node.sexp())
    canonical_actual = str(sexpparse.parse(tree.root_node.sexp()))
    # print(canonical_actual)
    canonical_expected = str(sexpparse.parse(expected_sexp))
    if canonical_actual == canonical_expected:
        continue

    print(f'[bold red]{test_name}[/]')
    print()
    print(escape(str(code, 'utf-8')))
    print()
    print_diff(canonical_expected, canonical_actual)
    print()
    update = Confirm.ask(
        f'[yellow]>[/yellow] update [b]{test_name}[/b]?',
        default = False
    )
    if update:
        tests[test_name][1] = canonical_actual
        updates += 1
    else:
        tests[test_name][1] = canonical_expected
        unupdated_failures += 1

if updates:
    with open(filename, 'w') as f:
        for test_name, (code, expected_sexp) in tests.items():
            f.write("==========\n")
            f.write(test_name + '\n')
            f.write("==========\n")
            f.write(str(code, 'utf-8') + '\n')
            f.write("---" + '\n')
            f.write(str(expected_sexp) + '\n')
    print(f'[bold green]updated {updates} tests![/]')

if unupdated_failures:
    print(f"{unupdated_failures} tests will still fail")
    exit(1)
