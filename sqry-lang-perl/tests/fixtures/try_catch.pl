package Grp::TryCatch;

use strict;
use warnings;
use feature 'try';
no warnings 'experimental::try';

sub attempt {
    try {
        risky();
    }
    catch ($error) {
        recover($error);
    }
    return 1;
}

sub risky {
    return 5;
}

sub recover {
    my ($error) = @_;
    return $error;
}
