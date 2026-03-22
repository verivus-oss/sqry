import re
from typing import List

class TokenizeError(Exception):
    pass

def tokenize(s: str):
    i = 0
    tokens = []
    current_word = ''
    while i < len(s):
        if s[i] == '(':
            if current_word:
                raise TokenizeError('expected word')
            tokens.append('(')
        elif s[i] == ')':
            if current_word:
                tokens.append(current_word)
                current_word = ''
            tokens.append(')')
        elif s[i] == ':':
            if current_word:
                tokens.append(current_word)
                current_word = ''
            tokens.append(':')
        elif re.match(r'\s', s[i]):
            if current_word:
                tokens.append(current_word)
                current_word = ''
        elif re.match(r"['a-zA-Z0-9_]", s[i]):
            current_word += s[i]
        else:
            raise TokenizeError(f'unexpected character `{s[i]}` (index {i})')
        i += 1
    return tokens

class Node:
    def __init__(self, name, label=None, arg=None):
        self.name: str = name
        self.label: str | None = label
        self.children: List[Node] = []
        self.arg: str | None = arg

    def _str_prefix(self):
        s = ''
        if self.label:
            s += self.label + ': '
        s += '('
        s += self.name
        if self.arg:
            s += ' ' + self.arg
        return s

    def __str__(self):
        s = self._str_prefix()
        if not self.children:
            return s + ')'
        else:
            for child in self.children:
                child_str = str(child)
                s += '\n' + '\n'.join('  ' + s for s in child_str.split('\n'))
            s += ')'
            return s


class ParseError(Exception):
    pass

def _parse(
    tokens: List[str],
    label: str | None = None,
    arg: str | None = None,
) -> Node:
    if tokens.pop(0) != '(':
        raise ParseError('expected (')
    if not re.match(r'[a-zA-Z0-9_]+', tokens[0]):
        raise ParseError(f'expected symbol, got {tokens[0]}')
    node = Node(tokens.pop(0), label)
    next_label = None
    while tokens:
        if tokens[0] == ')':
            tokens.pop(0)
            return node
        elif tokens[0] == '(':
            node.children.append(_parse(
                tokens,
                next_label,
            ))
            next_label = None
        elif tokens[1] == ':':
            next_label = tokens.pop(0)
            tokens.pop(0)
        elif tokens[0].startswith('\''): # (UNEXPECTED 'x')
            node.arg = tokens.pop(0)
        else:
            raise ParseError(f'unexpected token `{tokens[0]}`')
    raise ParseError('expected )')


# s = open('test_sexpr').read()
# print(s.__repr__())
# print(tokenize(s))
# print(parse(tokenize(s)))

def parse(sexpr: str):
    return _parse(tokenize(sexpr))

