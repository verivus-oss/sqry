import type { Config } from "./types";
import defaultLogger from "./logger";
import { util as utilAlias, compute } from "./util";
import * as Helpers from "./helpers";
import type * as TypeBag from "./bag";

defaultLogger();
Helpers.bootstrap();
