#!/usr/bin/env python3
"""
Temperature scaling calibration for sqry-nl intent classifier.

Applies post-hoc temperature scaling to improve confidence calibration.
The temperature parameter is learned on a held-out calibration set.

AC-11.7: Temperature scaling calibration produces ECE < 0.1 (C3 constraint)

Usage:
    python calibrate.py --model models/onnx/model.onnx --data data/calibration.json
    python calibrate.py --model models/onnx/model.onnx --data data/calibration.json --output models/temperature.json
"""

import json
from pathlib import Path
from typing import Optional

import numpy as np
from scipy.optimize import minimize_scalar
from scipy.special import softmax
from transformers import AutoTokenizer
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
MODEL_PATH_HELP = "ONNX model path"
TOKENIZER_DIR_HELP = "Tokenizer directory"


def load_model_and_tokenizer(
    model_path: Path, tokenizer_dir: Path
) -> tuple[ort.InferenceSession, AutoTokenizer]:
    session = ort.InferenceSession(str(model_path))
    tokenizer = AutoTokenizer.from_pretrained(tokenizer_dir)
    return session, tokenizer


def load_samples(data_path: Path) -> tuple[list[str], np.ndarray, int]:
    with open(data_path, encoding="utf-8") as file:
        data = json.load(file)

    samples = data["samples"]
    texts = [sample["text"] for sample in samples]
    label_ids = np.array([LABEL_TO_ID[sample["intent"]] for sample in samples])
    return texts, label_ids, len(samples)


def normalize_label_ids(labels: np.ndarray) -> np.ndarray:
    labels_array = np.asarray(labels)
    if isinstance(labels_array.flat[0], str):
        return np.array([LABEL_TO_ID[label] for label in labels_array.tolist()])
    return labels_array.astype(int, copy=False)


def compute_ece(
    confidences: np.ndarray,
    predictions: np.ndarray,
    labels: np.ndarray,
    n_bins: int = 15,
) -> float:
    """
    Compute Expected Calibration Error (ECE).

    ECE measures the difference between predicted confidence and actual accuracy
    across bins of predicted confidence.

    Args:
        confidences: Model's predicted confidence for each sample
        predictions: Model's predicted class for each sample
        labels: True labels
        n_bins: Number of bins for grouping confidences

    Returns:
        ECE value (lower is better, 0 = perfectly calibrated)
    """
    bin_boundaries = np.linspace(0, 1, n_bins + 1)
    ece = 0.0
    for i in range(n_bins):
        bin_lower = bin_boundaries[i]
        bin_upper = bin_boundaries[i + 1]

        # Get samples in this bin
        in_bin = (confidences > bin_lower) & (confidences <= bin_upper)
        prop_in_bin = in_bin.mean()

        if prop_in_bin > 0:
            # Average accuracy in bin
            accuracy_in_bin = (predictions[in_bin] == labels[in_bin]).mean()
            # Average confidence in bin
            avg_confidence_in_bin = confidences[in_bin].mean()
            # Weighted absolute difference
            ece += np.abs(avg_confidence_in_bin - accuracy_in_bin) * prop_in_bin

    return ece


def compute_mce(
    confidences: np.ndarray,
    predictions: np.ndarray,
    labels: np.ndarray,
    n_bins: int = 15,
) -> float:
    """
    Compute Maximum Calibration Error (MCE).

    MCE is the maximum gap between confidence and accuracy across bins.

    Args:
        confidences: Model's predicted confidence for each sample
        predictions: Model's predicted class for each sample
        labels: True labels
        n_bins: Number of bins for grouping confidences

    Returns:
        MCE value (lower is better)
    """
    bin_boundaries = np.linspace(0, 1, n_bins + 1)
    mce = 0.0

    for i in range(n_bins):
        bin_lower = bin_boundaries[i]
        bin_upper = bin_boundaries[i + 1]

        in_bin = (confidences > bin_lower) & (confidences <= bin_upper)

        if in_bin.sum() > 0:
            accuracy_in_bin = (predictions[in_bin] == labels[in_bin]).mean()
            avg_confidence_in_bin = confidences[in_bin].mean()
            gap = np.abs(avg_confidence_in_bin - accuracy_in_bin)
            mce = max(mce, gap)

    return mce


def temperature_scale(logits: np.ndarray, temperature: float) -> np.ndarray:
    """Apply temperature scaling to logits."""
    return logits / temperature


def find_optimal_temperature(
    logits: np.ndarray,
    labels: np.ndarray,
    bounds: tuple[float, float] = (0.1, 10.0),
) -> float:
    """
    Find optimal temperature using NLL minimization.

    Args:
        logits: Raw model logits (N, num_classes)
        labels: True labels (N,)
        bounds: Search bounds for temperature

    Returns:
        Optimal temperature value
    """
    labels = normalize_label_ids(labels)

    def nll_loss(temperature: float) -> float:
        """Negative log-likelihood with temperature scaling."""
        scaled_logits = temperature_scale(logits, temperature)
        probs = softmax(scaled_logits, axis=1)
        # Clip to avoid log(0)
        probs = np.clip(probs, 1e-10, 1.0)
        # NLL for true labels
        nll = -np.log(probs[np.arange(len(labels)), labels]).mean()
        return nll

    # Minimize NLL
    result = minimize_scalar(nll_loss, bounds=bounds, method="bounded")
    return result.x


def get_model_predictions(
    session: ort.InferenceSession,
    tokenizer: AutoTokenizer,
    texts: list[str],
    max_length: int = 128,
) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    """
    Get model predictions for a list of texts.

    Returns:
        logits: Raw logits (N, num_classes)
        predictions: Predicted classes (N,)
        confidences: Predicted confidences (N,)
    """
    all_logits = []

    for text in texts:
        inputs = tokenizer(
            text,
            return_tensors="np",
            truncation=True,
            max_length=max_length,
        )

        input_names = [i.name for i in session.get_inputs()]
        onnx_inputs = {
            "input_ids": inputs["input_ids"],
            "attention_mask": inputs["attention_mask"],
        }
        if "token_type_ids" in input_names and "token_type_ids" in inputs:
            onnx_inputs["token_type_ids"] = inputs["token_type_ids"]

        outputs = session.run(None, onnx_inputs)
        logits = outputs[0][0]  # Shape: (num_classes,)
        all_logits.append(logits)

    logits = np.array(all_logits)
    probs = softmax(logits, axis=1)
    predictions = np.argmax(probs, axis=1)
    confidences = np.max(probs, axis=1)

    return logits, predictions, confidences


def plot_reliability_diagram(
    confidences: np.ndarray,
    predictions: np.ndarray,
    labels: np.ndarray,
    n_bins: int = 10,
    title: str = "Reliability Diagram",
) -> str:
    """
    Generate ASCII reliability diagram.

    Returns:
        ASCII art representation of the reliability diagram
    """
    bin_boundaries = np.linspace(0, 1, n_bins + 1)
    bin_accuracies = []
    bin_confidences = []
    bin_counts = []

    for i in range(n_bins):
        bin_lower = bin_boundaries[i]
        bin_upper = bin_boundaries[i + 1]

        in_bin = (confidences > bin_lower) & (confidences <= bin_upper)
        count = in_bin.sum()
        bin_counts.append(count)

        if count > 0:
            accuracy = (predictions[in_bin] == labels[in_bin]).mean()
            avg_conf = confidences[in_bin].mean()
        else:
            accuracy = 0
            avg_conf = (bin_lower + bin_upper) / 2

        bin_accuracies.append(accuracy)
        bin_confidences.append(avg_conf)

    # Build ASCII diagram
    lines = [title, "=" * len(title), ""]
    lines.append("Confidence | Accuracy | Gap    | Count")
    lines.append("-" * 45)

    for i in range(n_bins):
        conf = bin_confidences[i]
        acc = bin_accuracies[i]
        count = bin_counts[i]
        gap = conf - acc

        # Visual accuracy bar
        bar_len = int(acc * 20)
        accuracy_bar = "#" * bar_len + "." * (20 - bar_len)

        lines.append(
            f"  {conf:.2f}     |   {acc:.2f}   | {gap:+.2f}  | {count:4d}  {accuracy_bar}"
        )

    return "\n".join(lines)


@app.command()
def calibrate(
    model_path: Path = typer.Option(
        ...,
        "--model",
        help=MODEL_PATH_HELP,
    ),
    tokenizer_dir: Path = typer.Option(
        ...,
        "--tokenizer",
        help=TOKENIZER_DIR_HELP,
    ),
    data_path: Path = typer.Option(
        ...,
        "--data",
        help="Calibration data JSON file",
    ),
    output: Path = typer.Option(
        Path("models/temperature.json"),
        "--output",
        help="Output file for temperature parameter",
    ),
) -> None:
    """Calibrate model confidence using temperature scaling."""
    console.print("[bold blue]sqry-nl Confidence Calibration[/bold blue]")
    console.print()

    with Progress(
        SpinnerColumn(),
        TextColumn("[progress.description]{task.description}"),
        console=console,
    ) as progress:
        # Load model and tokenizer
        task = progress.add_task("Loading model and data...", total=None)
        session, tokenizer = load_model_and_tokenizer(model_path, tokenizer_dir)
        texts, true_label_ids, sample_count = load_samples(data_path)
        console.print(f"  Loaded {sample_count} calibration samples")

        # Get model predictions
        progress.update(task, description="Getting model predictions...")
        logits, predictions, confidences = get_model_predictions(
            session, tokenizer, texts
        )

        # Compute metrics before calibration
        progress.update(task, description="Computing pre-calibration metrics...")
        ece_before = compute_ece(confidences, predictions, true_label_ids)
        mce_before = compute_mce(confidences, predictions, true_label_ids)
        accuracy = (predictions == true_label_ids).mean()

        console.print()
        console.print("[bold]Pre-Calibration Metrics:[/bold]")
        console.print(f"  Accuracy: {accuracy:.4f}")
        console.print(f"  ECE: {ece_before:.4f}")
        console.print(f"  MCE: {mce_before:.4f}")

        # Find optimal temperature
        progress.update(task, description="Finding optimal temperature...")
        optimal_temp = find_optimal_temperature(logits, true_label_ids)

        console.print()
        console.print(f"[bold]Optimal Temperature:[/bold] {optimal_temp:.4f}")

        # Apply temperature scaling and recompute metrics
        progress.update(task, description="Applying temperature scaling...")
        scaled_logits = temperature_scale(logits, optimal_temp)
        scaled_probs = softmax(scaled_logits, axis=1)
        scaled_confidences = np.max(scaled_probs, axis=1)
        scaled_predictions = np.argmax(scaled_probs, axis=1)

        ece_after = compute_ece(scaled_confidences, scaled_predictions, true_label_ids)
        mce_after = compute_mce(scaled_confidences, scaled_predictions, true_label_ids)

        console.print()
        console.print("[bold]Post-Calibration Metrics:[/bold]")
        console.print(f"  ECE: {ece_after:.4f} (was {ece_before:.4f})")
        console.print(f"  MCE: {mce_after:.4f} (was {mce_before:.4f})")
        if ece_before > 0:
            console.print(f"  ECE improvement: {(ece_before - ece_after) / ece_before * 100:.1f}%")
        else:
            console.print("  ECE improvement: N/A (model was already perfectly calibrated)")

        # Check AC-11.7: ECE < 0.1
        if ece_after < 0.1:
            console.print("[green]AC-11.7: ECE < 0.1 satisfied[/green]")
        else:
            console.print(f"[red]WARNING: ECE {ece_after:.4f} >= 0.1 (AC-11.7 violation)[/red]")

        # Show reliability diagrams
        progress.update(task, description="Generating reliability diagrams...")

    console.print()
    console.print("[bold]Reliability Diagram (Before):[/bold]")
    diagram_before = plot_reliability_diagram(
        confidences, predictions, true_label_ids, title="Before Calibration"
    )
    console.print(diagram_before)

    console.print()
    console.print("[bold]Reliability Diagram (After):[/bold]")
    diagram_after = plot_reliability_diagram(
        scaled_confidences, scaled_predictions, true_label_ids, title="After Calibration"
    )
    console.print(diagram_after)

    # Save temperature parameter
    output.parent.mkdir(parents=True, exist_ok=True)
    calibration_data = {
        "temperature": optimal_temp,
        "metrics": {
            "accuracy": float(accuracy),
            "ece_before": float(ece_before),
            "ece_after": float(ece_after),
            "mce_before": float(mce_before),
            "mce_after": float(mce_after),
        },
        "calibration_samples": sample_count,
        "intent_labels": INTENT_LABELS,
    }

    with open(output, "w") as f:
        json.dump(calibration_data, f, indent=2)

    console.print()
    console.print(f"[green]Temperature parameter saved to: {output}[/green]")
    console.print()
    console.print("[dim]Usage in Rust:[/dim]")
    console.print(f"  let temperature: f32 = {optimal_temp:.4f};")
    console.print("  let calibrated_probs = softmax(logits / temperature);")


@app.command()
def analyze(
    model_path: Path = typer.Option(
        ...,
        "--model",
        help=MODEL_PATH_HELP,
    ),
    tokenizer_dir: Path = typer.Option(
        ...,
        "--tokenizer",
        help=TOKENIZER_DIR_HELP,
    ),
    data_path: Path = typer.Option(
        ...,
        "--data",
        help="Data JSON file to analyze",
    ),
    temperature: Optional[float] = typer.Option(
        None,
        "--temperature",
        help="Temperature for scaling (from temperature.json)",
    ),
) -> None:
    """Analyze model calibration on a dataset."""
    console.print("[bold blue]sqry-nl Calibration Analysis[/bold blue]")
    console.print()

    # Load model and data
    session, tokenizer = load_model_and_tokenizer(model_path, tokenizer_dir)
    texts, true_label_ids, sample_count = load_samples(data_path)

    # Get predictions
    logits, predictions, confidences = get_model_predictions(session, tokenizer, texts)

    # Apply temperature if provided
    if temperature:
        console.print(f"Applying temperature: {temperature}")
        logits = temperature_scale(logits, temperature)
        probs = softmax(logits, axis=1)
        confidences = np.max(probs, axis=1)
        predictions = np.argmax(probs, axis=1)

    # Compute metrics
    accuracy = (predictions == true_label_ids).mean()
    ece = compute_ece(confidences, predictions, true_label_ids)
    mce = compute_mce(confidences, predictions, true_label_ids)

    console.print(f"[bold]Dataset:[/bold] {data_path}")
    console.print(f"[bold]Samples:[/bold] {sample_count}")
    console.print()
    console.print("[bold]Metrics:[/bold]")
    console.print(f"  Accuracy: {accuracy:.4f}")
    console.print(f"  ECE: {ece:.4f}")
    console.print(f"  MCE: {mce:.4f}")
    console.print(f"  Avg Confidence: {confidences.mean():.4f}")

    # Per-class analysis
    console.print()
    console.print("[bold]Per-Class Analysis:[/bold]")
    for i, label in enumerate(INTENT_LABELS):
        mask = true_label_ids == i
        if mask.sum() > 0:
            class_acc = (predictions[mask] == i).mean()
            class_conf = confidences[mask].mean()
            class_count = mask.sum()
            console.print(f"  {label:15s}: acc={class_acc:.3f}, conf={class_conf:.3f}, n={class_count}")

    # Reliability diagram
    console.print()
    diagram = plot_reliability_diagram(
        confidences, predictions, true_label_ids, title="Reliability Diagram"
    )
    console.print(diagram)


@app.command()
def test_single(
    model_path: Path = typer.Argument(..., help=MODEL_PATH_HELP),
    tokenizer_dir: Path = typer.Option(
        ...,
        "--tokenizer",
        help=TOKENIZER_DIR_HELP,
    ),
    text: str = typer.Argument(..., help="Text to classify"),
    temperature: float = typer.Option(
        1.0,
        "--temperature",
        help="Temperature for scaling",
    ),
) -> None:
    """Test calibration on a single input."""
    session, tokenizer = load_model_and_tokenizer(model_path, tokenizer_dir)

    # Get prediction
    inputs = tokenizer(text, return_tensors="np", truncation=True, max_length=128)
    onnx_inputs = {
        "input_ids": inputs["input_ids"],
        "attention_mask": inputs["attention_mask"],
    }
    outputs = session.run(None, onnx_inputs)
    logits = outputs[0][0]

    # Apply temperature scaling
    scaled_logits = logits / temperature
    probs = softmax(scaled_logits)

    pred_id = np.argmax(probs)
    confidence = probs[pred_id]

    console.print(f"[bold]Input:[/bold] {text}")
    console.print(f"[bold]Temperature:[/bold] {temperature}")
    console.print(f"[bold]Prediction:[/bold] {INTENT_LABELS[pred_id]}")
    console.print(f"[bold]Confidence:[/bold] {confidence:.4f}")
    console.print()
    console.print("[bold]All Probabilities:[/bold]")
    for i, label in enumerate(INTENT_LABELS):
        probability_bar = "#" * int(probs[i] * 40)
        console.print(f"  {label:15s}: {probs[i]:.4f} {probability_bar}")


if __name__ == "__main__":
    app()
