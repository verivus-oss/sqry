# Hand-written Terraform sample for body-shape descriptor coverage.
# Terraform is purely declarative: it has no function/method definitions, only
# blocks and expression-level constructs (a `for` comprehension and a conditional
# below). The shape coverage test asserts that no eligible Function/Method node
# with a body span is emitted, so the build seam attaches no descriptor here.
variable "environments" {
  type    = list(string)
  default = ["dev", "staging", "prod"]
}

locals {
  upper_envs = [for e in var.environments : upper(e) if e != "dev"]
  primary    = length(var.environments) > 0 ? var.environments[0] : "none"
}

module "network" {
  source = "./modules/network"
  cidr   = "10.0.0.0/16"
}

resource "aws_s3_bucket" "logs" {
  bucket = "myapp-${local.primary}-logs"
}
