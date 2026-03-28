<?php

use App\Services\Mailer;
use App\{Jobs\SendEmail, Helpers as CoreHelpers};
use function App\Utils\array_flatten;
use const App\Config\VERSION;

require __DIR__ . '/bootstrap.php';
require_once __DIR__ . '/config/constants.php';
include 'helpers.php';
include_once 'polyfills.php';
