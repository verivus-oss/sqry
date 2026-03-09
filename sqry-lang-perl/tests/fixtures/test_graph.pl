#!/usr/bin/env perl
# Test fixture for Perl GraphBuilder
# Tests packages, subroutines, methods, and function calls

package MyApp::Utils;

use strict;
use warnings;

# Simple subroutine
sub helper {
    my ($arg) = @_;
    return $arg * 2;
}

# Sub calling another sub
sub calculate {
    my ($x, $y) = @_;
    my $result = helper($x);
    return $result + $y;
}

package MyApp::Service;

use strict;
use warnings;

# Method declaration
sub process {
    my ($self, $data) = @_;
    # Call to Utils package
    MyApp::Utils::helper($data);
}

# Method with signature (Perl 5.20+)
method validate ($self, $input) {
    return 1 if $input > 0;
    return 0;
}

package main;

# Main package functions
sub run {
    my $service = MyApp::Service->new();
    $service->process(42);
}

sub startup {
    run();
}

1;
