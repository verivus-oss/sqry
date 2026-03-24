# Comprehensive Terraform fixture for testing symbol extraction

# Resource blocks
resource "aws_s3_bucket" "my_bucket" {
  bucket = "example-bucket-name"
  acl    = "private"
}

resource "aws_instance" "web_server" {
  ami           = "ami-12345678"
  instance_type = var.instance_type
}

# Data source
data "aws_ami" "ubuntu" {
  most_recent = true

  filter {
    name   = "name"
    values = ["ubuntu/images/hvm-ssd/ubuntu-focal-20.04-amd64-server-*"]
  }
}

# Module declaration
module "vpc" {
  source = "./modules/vpc"

  vpc_name = "main-vpc"
  cidr     = "10.0.0.0/16"
}

module "external_module" {
  source  = "terraform-aws-modules/vpc/aws"
  version = "3.0.0"
}

# Variables
variable "instance_type" {
  type        = string
  description = "EC2 instance type"
  default     = "t2.micro"
}

variable "region" {
  type = string
}

# Output values
output "bucket_name" {
  value       = aws_s3_bucket.my_bucket.id
  description = "The name of the S3 bucket"
}

output "instance_ip" {
  value = aws_instance.web_server.public_ip
}

# Provider configuration
provider "aws" {
  region = var.region
}

# Locals block
locals {
  environment = "production"
  common_tags = {
    Environment = local.environment
    ManagedBy   = "Terraform"
  }
  app_name = "myapp"
}
