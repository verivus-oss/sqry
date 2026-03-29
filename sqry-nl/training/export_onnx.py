#!/usr/bin/env python3
"""
ONNX export and int8 quantization for sqry-nl intent classifier.

Converts the trained PyTorch model to ONNX format and applies dynamic
int8 quantization for efficient inference.

AC-11.6: Int8 quantization accuracy drop <2% (C2 constraint)

Usage:
    python export_onnx.py --model models/intent_classifier/final
    python export_onnx.py --model models/intent_classifier/final --quantize
"""

import json
import hashlib
from pathlib import Path
from typing import Optional

import numpy as np
import torch
from transformers import AutoModelForSequenceClassification, AutoTokenizer
from optimum.onnxruntime import ORTModelForSequenceClassification
from optimum.onnxruntime.configuration import OptimizationConfig, QuantizationConfig
from optimum.onnxruntime import ORTQuantizer, ORTOptimizer
from onnxruntime.quantization import QuantFormat, QuantType, QuantizationMode
import onnx
import onnxruntime as ort
import typer
from rich.console import Console
from rich.progress import Progress, SpinnerColumn, TextColumn

console = Console()
app = typer.Typer()

# Intent labels (must match train_classifier.py and sqry-nl/src/types.rs)
INTENT_LABELS = [
    "SymbolQuery",
    "TextSearch",
    "TracePath",
    "FindCallers",
    "FindCallees",
    "Visualize",
    "IndexStatus",
    "Ambiguous",
]
LABEL_TO_ID = {label: i for i, label in enumerate(INTENT_LABELS)}
DEFAULT_TEST_TEXTS = [
    "find authenticate_user",
    "who calls login",
    "grep for TODO",
    "help",
]


def load_local_tokenizer(tokenizer_dir: Path) -> AutoTokenizer:
    return AutoTokenizer.from_pretrained(tokenizer_dir, local_files_only=True)


def load_local_pytorch_model(
    model_dir: Path,
) -> AutoModelForSequenceClassification:
    return AutoModelForSequenceClassification.from_pretrained(
        model_dir,
        local_files_only=True,
    )


def build_onnx_inputs(
    tokenizer: AutoTokenizer,
    text: str,
    max_length: int = 128,
) -> dict[str, np.ndarray]:
    inputs = tokenizer(
        text,
        return_tensors="np",
        truncation=True,
        max_length=max_length,
    )
    result = {
        "input_ids": inputs["input_ids"],
        "attention_mask": inputs["attention_mask"],
    }
    if "token_type_ids" in inputs:
        result["token_type_ids"] = inputs["token_type_ids"]
    return result


def compute_sha256(file_path: Path) -> str:
    """Compute SHA256 hash of a file."""
    sha256_hash = hashlib.sha256()
    with open(file_path, "rb") as f:
        for byte_block in iter(lambda: f.read(4096), b""):
            sha256_hash.update(byte_block)
    return sha256_hash.hexdigest()


def verify_onnx_model(model_path: Path) -> bool:
    """Verify ONNX model is valid."""
    try:
        model = onnx.load(str(model_path))
        onnx.checker.check_model(model)
        return True
    except Exception as e:
        console.print(f"[red]ONNX validation failed: {e}[/red]")
        return False


def compare_outputs(
    pytorch_model: AutoModelForSequenceClassification,
    tokenizer: AutoTokenizer,
    onnx_session: ort.InferenceSession,
    test_texts: list[str],
    tolerance: float = 1e-4,
) -> tuple[bool, float]:
    """Compare PyTorch and ONNX model outputs."""
    pytorch_model.eval()
    max_diff = 0.0

    for text in test_texts:
        # Tokenize
        inputs = tokenizer(
            text,
            return_tensors="pt",
            truncation=True,
            max_length=128,
        )

        # PyTorch inference
        with torch.no_grad():
            pytorch_outputs = pytorch_model(**inputs)
            pytorch_logits = pytorch_outputs.logits.numpy()

        # ONNX inference
        input_names = [i.name for i in onnx_session.get_inputs()]
        onnx_inputs = {
            "input_ids": inputs["input_ids"].numpy(),
            "attention_mask": inputs["attention_mask"].numpy(),
        }
        if "token_type_ids" in input_names and "token_type_ids" in inputs:
            onnx_inputs["token_type_ids"] = inputs["token_type_ids"].numpy()
        onnx_outputs = onnx_session.run(None, onnx_inputs)
        onnx_logits = onnx_outputs[0]

        # Compare
        diff = np.abs(pytorch_logits - onnx_logits).max()
        max_diff = max(max_diff, diff)

    is_valid = max_diff < tolerance
    return is_valid, max_diff


def evaluate_accuracy(
    onnx_session: ort.InferenceSession,
    tokenizer: AutoTokenizer,
    eval_data_path: Path,
) -> float:
    """Evaluate ONNX model accuracy on eval data."""
    with open(eval_data_path, encoding="utf-8") as file:
        data = json.load(file)

    samples = data["samples"]
    if not samples:
        raise ValueError("evaluation dataset contains no samples")
    correct = 0

    for sample in samples:
        text = sample["text"]
        true_label = LABEL_TO_ID[sample["intent"]]

        # Tokenize
        onnx_inputs = build_onnx_inputs(tokenizer, text)
        outputs = onnx_session.run(None, onnx_inputs)
        logits = outputs[0]
        pred_label = np.argmax(logits, axis=-1)[0]

        if pred_label == true_label:
            correct += 1

    return correct / len(samples)


def _optimize_onnx_model(output_dir: Path) -> Path:
    """Apply ONNX graph optimizations and return optimized directory."""
    optimizer = ORTOptimizer.from_pretrained(output_dir)
    optimization_config = OptimizationConfig(
        optimization_level=99,
        optimize_for_gpu=False,
    )
    optimized_dir = output_dir / "optimized"
    optimizer.optimize(
        save_dir=optimized_dir,
        optimization_config=optimization_config,
    )
    console.print(f"  Optimized model saved to: {optimized_dir}")
    return optimized_dir


def _quantize_onnx_model(output_dir: Path, onnx_path: Path) -> Optional[Path]:
    """Apply int8 quantization and return quantized path."""
    quantizer = ORTQuantizer.from_pretrained(output_dir)
    quantization_config = QuantizationConfig(
        is_static=False,
        format=QuantFormat.QOperator,
        mode=QuantizationMode.IntegerOps,
        per_channel=True,
        weights_dtype=QuantType.QInt8,
        operators_to_quantize=["MatMul"],
    )
    quantized_dir = output_dir / "quantized"
    quantizer.quantize(
        save_dir=quantized_dir,
        quantization_config=quantization_config,
    )
    quantized_onnx_path = quantized_dir / "model.onnx"
    console.print(f"  Quantized model saved to: {quantized_dir}")

    # Report size reduction
    original_size = onnx_path.stat().st_size / (1024 * 1024)
    if quantized_onnx_path.exists():
        quantized_size = quantized_onnx_path.stat().st_size / (1024 * 1024)
        reduction = (1 - quantized_size / original_size) * 100
        console.print(f"  Original size: {original_size:.2f} MB")
        console.print(f"  Quantized size: {quantized_size:.2f} MB")
        console.print(f"  Size reduction: {reduction:.1f}%")

    return quantized_onnx_path


def _evaluate_quantized_accuracy(
    onnx_path: Path,
    quantized_onnx_path: Optional[Path],
    tokenizer: AutoTokenizer,
    eval_data: Path,
) -> None:
    """Evaluate and compare original vs quantized model accuracy."""
    original_session = ort.InferenceSession(str(onnx_path))
    original_accuracy = evaluate_accuracy(original_session, tokenizer, eval_data)
    console.print(f"  Original ONNX accuracy: {original_accuracy:.4f}")

    if quantized_onnx_path is not None and quantized_onnx_path.exists():
        quantized_session = ort.InferenceSession(str(quantized_onnx_path))
        quantized_accuracy = evaluate_accuracy(quantized_session, tokenizer, eval_data)
        accuracy_drop = (original_accuracy - quantized_accuracy) * 100
        console.print(f"  Quantized accuracy: {quantized_accuracy:.4f}")
        console.print(f"  Accuracy drop: {accuracy_drop:.2f}%")

        if accuracy_drop > 2.0:
            console.print("[red]WARNING: Accuracy drop exceeds 2% (AC-11.6 violation)[/red]")
        else:
            console.print("[green]AC-11.6: Accuracy drop within 2% limit[/green]")


@app.command()
def export(
    model_dir: Path = typer.Option(
        ...,
        "--model",
        help="PyTorch model directory",
    ),
    output_dir: Path = typer.Option(
        Path("models/onnx"),
        "--output",
        help="Output directory for ONNX model",
    ),
    quantize: bool = typer.Option(
        False,
        "--quantize/--no-quantize",
        help="Apply int8 dynamic quantization",
    ),
    optimize: bool = typer.Option(
        True,
        "--optimize/--no-optimize",
        help="Apply graph optimizations",
    ),
    eval_data: Optional[Path] = typer.Option(
        None,
        "--eval",
        help="Evaluation data for accuracy comparison",
    ),
) -> None:
    """Export PyTorch model to ONNX format."""
    console.print("[bold blue]sqry-nl ONNX Export[/bold blue]")
    console.print()

    output_dir.mkdir(parents=True, exist_ok=True)

    with Progress(
        SpinnerColumn(),
        TextColumn("[progress.description]{task.description}"),
        console=console,
    ) as progress:
        # Load PyTorch model
        task = progress.add_task("Loading PyTorch model...", total=None)
        # Use local_files_only=True to force loading from local directory
        tokenizer = load_local_tokenizer(model_dir)
        pytorch_model = load_local_pytorch_model(model_dir)
        quantized_onnx_path: Optional[Path] = None

        console.print(f"  Loaded model from: {model_dir}")

        # Export to ONNX using optimum
        progress.update(task, description="Exporting to ONNX...")
        onnx_model = ORTModelForSequenceClassification.from_pretrained(
            model_dir,
            export=True,
            local_files_only=True,
        )

        # Save initial ONNX model
        onnx_path = output_dir / "model.onnx"
        onnx_model.save_pretrained(output_dir)
        tokenizer.save_pretrained(output_dir)

        console.print(f"  Exported ONNX model to: {output_dir}")

        # Verify ONNX model
        progress.update(task, description="Verifying ONNX model...")
        if not verify_onnx_model(onnx_path):
            console.print("[red]ONNX model verification failed![/red]")
            raise typer.Exit(1)

        console.print("  [green]ONNX model valid[/green]")

        # Compare outputs
        progress.update(task, description="Comparing PyTorch vs ONNX outputs...")
        onnx_session = ort.InferenceSession(str(onnx_path))
        is_valid, max_diff = compare_outputs(
            pytorch_model, tokenizer, onnx_session, DEFAULT_TEST_TEXTS
        )

        if is_valid:
            console.print(f"  [green]Output comparison passed (max diff: {max_diff:.6f})[/green]")
        else:
            console.print(f"  [yellow]Output comparison warning (max diff: {max_diff:.6f})[/yellow]")

        # Optimize if requested
        if optimize:
            progress.update(task, description="Optimizing ONNX graph...")
            _optimize_onnx_model(output_dir)

        # Quantize if requested
        if quantize:
            progress.update(task, description="Applying int8 quantization...")
            quantized_onnx_path = _quantize_onnx_model(output_dir, onnx_path)

        # Evaluate accuracy if eval data provided
        if eval_data is not None:
            progress.update(task, description="Evaluating accuracy...")
            _evaluate_quantized_accuracy(
                onnx_path,
                quantized_onnx_path if quantize else None,
                tokenizer,
                eval_data,
            )

        # Compute checksums (AC-11.8)
        progress.update(task, description="Computing checksums...")
        checksums = {}

        for model_file in output_dir.glob("**/*.onnx"):
            rel_path = model_file.relative_to(output_dir)
            checksums[str(rel_path)] = compute_sha256(model_file)

        # Save checksums
        with open(output_dir / "checksums.json", "w", encoding="utf-8") as f:
            json.dump(checksums, f, indent=2)

        console.print(f"  Checksums saved to: {output_dir / 'checksums.json'}")

    # Summary
    console.print()
    console.print("[bold]Export Summary:[/bold]")
    console.print(f"  ONNX model: {onnx_path}")
    if optimize:
        console.print(f"  Optimized model: {output_dir / 'optimized'}")
    if quantize:
        console.print(f"  Quantized model: {output_dir / 'quantized'}")

    console.print()
    console.print("[bold]Checksums (AC-11.8):[/bold]")
    for path, checksum in checksums.items():
        console.print(f"  {path}: {checksum[:16]}...")

    console.print()
    console.print("[dim]Next steps:[/dim]")
    console.print("  1. Calibrate confidence: python calibrate.py --model models/onnx/model.onnx")
    console.print("  2. Copy model files to sqry-nl/models/")


@app.command()
def verify(
    model_path: Path = typer.Argument(..., help="ONNX model path"),
) -> None:
    """Verify an ONNX model is valid."""
    console.print(f"[bold]Verifying: {model_path}[/bold]")

    if not model_path.exists():
        console.print(f"[red]File not found: {model_path}[/red]")
        raise typer.Exit(1)

    # Check ONNX validity
    if verify_onnx_model(model_path):
        console.print("[green]ONNX model is valid[/green]")
    else:
        console.print("[red]ONNX model is invalid[/red]")
        raise typer.Exit(1)

    # Print model info
    model = onnx.load(str(model_path))
    console.print()
    console.print("[bold]Model Info:[/bold]")
    console.print(f"  IR version: {model.ir_version}")
    console.print(f"  Opset version: {model.opset_import[0].version}")
    console.print(f"  Inputs: {[i.name for i in model.graph.input]}")
    console.print(f"  Outputs: {[o.name for o in model.graph.output]}")

    # File size
    size_mb = model_path.stat().st_size / (1024 * 1024)
    console.print(f"  Size: {size_mb:.2f} MB")

    # Checksum
    checksum = compute_sha256(model_path)
    console.print(f"  SHA256: {checksum}")


@app.command()
def benchmark(
    model_path: Path = typer.Argument(..., help="ONNX model path"),
    tokenizer_dir: Path = typer.Option(
        ...,
        "--tokenizer",
        help="Tokenizer directory",
    ),
    num_runs: int = typer.Option(100, "--runs", help="Number of benchmark runs"),
) -> None:
    """Benchmark ONNX model inference latency."""
    import time

    console.print(f"[bold]Benchmarking: {model_path}[/bold]")

    # Load model and tokenizer
    tokenizer = load_local_tokenizer(tokenizer_dir)
    session = ort.InferenceSession(str(model_path))

    # Test inputs
    test_texts = [
        "find authenticate_user",
        "who calls login function",
        "grep for TODO comments in the codebase",
        "show me the path from main to process_request",
    ]

    # Warmup
    for text in test_texts:
        onnx_inputs = build_onnx_inputs(tokenizer, text)
        session.run(None, onnx_inputs)

    # Benchmark
    latencies = []
    for _ in range(num_runs):
        text = test_texts[_ % len(test_texts)]
        onnx_inputs = build_onnx_inputs(tokenizer, text)

        start = time.perf_counter()
        session.run(None, onnx_inputs)
        end = time.perf_counter()

        latencies.append((end - start) * 1000)  # Convert to ms

    # Statistics
    latencies = np.array(latencies)
    console.print()
    console.print("[bold]Latency Statistics:[/bold]")
    console.print(f"  Mean: {np.mean(latencies):.2f} ms")
    console.print(f"  Std: {np.std(latencies):.2f} ms")
    console.print(f"  P50: {np.percentile(latencies, 50):.2f} ms")
    console.print(f"  P90: {np.percentile(latencies, 90):.2f} ms")
    console.print(f"  P99: {np.percentile(latencies, 99):.2f} ms")
    console.print(f"  Min: {np.min(latencies):.2f} ms")
    console.print(f"  Max: {np.max(latencies):.2f} ms")


if __name__ == "__main__":
    app()
