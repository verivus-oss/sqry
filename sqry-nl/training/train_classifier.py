#!/usr/bin/env python3
"""
MiniLM-L6-v2 intent classifier training for sqry-nl.

Fine-tunes MiniLM-L6-v2 on generated training data for intent classification.
Produces a PyTorch model that can be exported to ONNX with export_onnx.py.

Usage:
    python train_classifier.py --train data/train.json --eval data/eval.json
    python train_classifier.py --train data/train.json --output models/intent_classifier
"""

import json
from pathlib import Path
from dataclasses import dataclass
from typing import Optional

import numpy as np
import torch
from datasets import Dataset
from sklearn.metrics import accuracy_score, f1_score, classification_report
from transformers import (
    AutoModelForSequenceClassification,
    AutoTokenizer,
    DataCollatorWithPadding,
    EarlyStoppingCallback,
    EvalPrediction,
    Trainer,
    TrainingArguments,
)
import typer
from rich.console import Console
from rich.progress import Progress, SpinnerColumn, TextColumn

console = Console()
app = typer.Typer()

# Intent labels (must match sqry-nl/src/types.rs)
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

LABEL2ID = {label: i for i, label in enumerate(INTENT_LABELS)}
ID2LABEL = dict(enumerate(INTENT_LABELS))

# Model configuration
MODEL_NAME = "sentence-transformers/all-MiniLM-L6-v2"
MAX_LENGTH = 128


@dataclass
class TrainingConfig:
    """Training configuration."""

    model_name: str = MODEL_NAME
    max_length: int = MAX_LENGTH
    batch_size: int = 32
    learning_rate: float = 2e-5
    num_epochs: int = 3
    warmup_ratio: float = 0.1
    weight_decay: float = 0.01
    seed: int = 42
    fp16: bool = torch.cuda.is_available()
    gradient_accumulation_steps: int = 1


def load_training_data(file_path: Path) -> Dataset:
    """Load training data from JSON file."""
    with open(file_path, encoding="utf-8") as f:
        data = json.load(f)

    samples = data["samples"]

    # Convert to dataset format
    texts = [s["text"] for s in samples]
    labels = [LABEL2ID[s["intent"]] for s in samples]

    return Dataset.from_dict({"text": texts, "label": labels})


def compute_metrics(eval_pred: EvalPrediction) -> dict:
    """Compute evaluation metrics."""
    predictions = np.argmax(eval_pred.predictions, axis=1)
    labels = eval_pred.label_ids

    accuracy = accuracy_score(labels, predictions)
    f1_macro = f1_score(labels, predictions, average="macro")
    f1_weighted = f1_score(labels, predictions, average="weighted")

    # Per-class F1 scores
    per_class_f1 = f1_score(labels, predictions, average=None)
    per_class_metrics = {
        f"f1_{INTENT_LABELS[i]}": score for i, score in enumerate(per_class_f1)
    }

    return {
        "accuracy": accuracy,
        "f1_macro": f1_macro,
        "f1_weighted": f1_weighted,
        **per_class_metrics,
    }


def create_tokenizer(model_name: str = MODEL_NAME) -> AutoTokenizer:
    """Create tokenizer with configuration."""
    tokenizer = AutoTokenizer.from_pretrained(model_name)
    return tokenizer


def create_model(model_name: str = MODEL_NAME) -> AutoModelForSequenceClassification:
    """Create model for sequence classification."""
    model = AutoModelForSequenceClassification.from_pretrained(
        model_name,
        num_labels=len(INTENT_LABELS),
        id2label=ID2LABEL,
        label2id=LABEL2ID,
    )
    return model


def tokenize_dataset(
    dataset: Dataset,
    tokenizer: AutoTokenizer,
    max_length: int = MAX_LENGTH,
) -> Dataset:
    """Tokenize dataset."""

    def tokenize_fn(examples):
        return tokenizer(
            examples["text"],
            truncation=True,
            max_length=max_length,
            padding=False,  # DataCollator handles padding
        )

    return dataset.map(tokenize_fn, batched=True, remove_columns=["text"])


def _create_training_args(
    output_dir: Path,
    num_epochs: int,
    batch_size: int,
    learning_rate: float,
    lr_scheduler_type: str,
    warmup_ratio: float,
    fp16: bool,
    eval_dataset: Optional[Dataset],
    seed: int,
) -> TrainingArguments:
    """Create TrainingArguments with the given configuration."""
    return TrainingArguments(
        output_dir=str(output_dir),
        overwrite_output_dir=True,
        num_train_epochs=num_epochs,
        per_device_train_batch_size=batch_size,
        per_device_eval_batch_size=batch_size,
        learning_rate=learning_rate,
        lr_scheduler_type=lr_scheduler_type,
        warmup_ratio=warmup_ratio,
        weight_decay=0.01,
        fp16=fp16,
        logging_dir=str(output_dir / "logs"),
        logging_steps=100,
        eval_strategy="epoch" if eval_dataset else "no",
        save_strategy="epoch",
        save_total_limit=2,
        load_best_model_at_end=eval_dataset is not None,
        metric_for_best_model="f1_macro" if eval_dataset else None,
        greater_is_better=True if eval_dataset else None,
        report_to="none",
        seed=seed,
    )


def _print_training_config(
    model_name: str,
    batch_size: int,
    learning_rate: float,
    lr_scheduler_type: str,
    warmup_ratio: float,
    num_epochs: int,
    fp16: bool,
    early_stopping_patience: Optional[int],
) -> None:
    """Print training configuration summary."""
    console.print()
    console.print("[bold]Starting training...[/bold]")
    console.print(f"  Model: {model_name}")
    console.print(f"  Batch size: {batch_size}")
    console.print(f"  Learning rate: {learning_rate}")
    console.print(f"  LR scheduler: {lr_scheduler_type}")
    console.print(f"  Warmup ratio: {warmup_ratio}")
    console.print(f"  Epochs: {num_epochs}")
    console.print(f"  FP16: {fp16}")
    if early_stopping_patience is not None:
        console.print(f"  Early stopping: patience={early_stopping_patience}")
    console.print()


def _run_final_evaluation(trainer: Trainer, eval_dataset: Dataset, output_dir: Path) -> None:
    """Run and print final evaluation results."""
    console.print()
    console.print("[bold]Final Evaluation:[/bold]")
    eval_results = trainer.evaluate()
    for key, value in sorted(eval_results.items()):
        if isinstance(value, float):
            console.print(f"  {key}: {value:.4f}")
        else:
            console.print(f"  {key}: {value}")

    # Detailed classification report
    console.print()
    console.print("[bold]Classification Report:[/bold]")
    predictions = trainer.predict(eval_dataset)
    preds = np.argmax(predictions.predictions, axis=1)
    labels = predictions.label_ids
    report = classification_report(labels, preds, target_names=INTENT_LABELS, digits=4)
    console.print(report)

    # Save evaluation metrics
    with open(output_dir / "eval_metrics.json", "w", encoding="utf-8") as f:
        json.dump(eval_results, f, indent=2)


@app.command()
def train(
    train_data: Path = typer.Option(
        ...,
        "--train",
        help="Training data JSON file",
    ),
    eval_data: Optional[Path] = typer.Option(
        None,
        "--eval",
        help="Evaluation data JSON file (optional)",
    ),
    output_dir: Path = typer.Option(
        Path("models/intent_classifier"),
        "--output",
        help="Output directory for model",
    ),
    model_name: str = typer.Option(
        MODEL_NAME,
        "--model",
        help="Base model name",
    ),
    batch_size: int = typer.Option(
        32,
        "--batch-size",
        help="Training batch size",
    ),
    learning_rate: float = typer.Option(
        2e-5,
        "--lr",
        help="Learning rate",
    ),
    num_epochs: int = typer.Option(
        3,
        "--epochs",
        help="Number of training epochs",
    ),
    seed: int = typer.Option(
        42,
        "--seed",
        help="Random seed",
    ),
    fp16: bool = typer.Option(
        torch.cuda.is_available(),
        "--fp16/--no-fp16",
        help="Use mixed precision training",
    ),
    early_stopping_patience: Optional[int] = typer.Option(
        None,
        "--early-stopping",
        help="Early stopping patience (epochs without improvement). Requires --eval.",
    ),
    lr_scheduler_type: str = typer.Option(
        "linear",
        "--lr-scheduler",
        help="LR scheduler type (linear, cosine, constant, constant_with_warmup)",
    ),
    warmup_ratio: float = typer.Option(
        0.1,
        "--warmup-ratio",
        help="Warmup ratio for LR scheduler",
    ),
) -> None:
    """Train MiniLM-L6-v2 intent classifier."""
    console.print("[bold blue]sqry-nl Intent Classifier Training[/bold blue]")
    console.print()

    # Set seed for reproducibility
    torch.manual_seed(seed)
    np.random.seed(seed)

    # Create output directory
    output_dir.mkdir(parents=True, exist_ok=True)

    with Progress(
        SpinnerColumn(),
        TextColumn("[progress.description]{task.description}"),
        console=console,
    ) as progress:
        # Load data
        task = progress.add_task("Loading training data...", total=None)
        train_dataset = load_training_data(train_data)
        console.print(f"  Loaded {len(train_dataset)} training samples")

        eval_dataset = None
        if eval_data:
            progress.update(task, description="Loading evaluation data...")
            eval_dataset = load_training_data(eval_data)
            console.print(f"  Loaded {len(eval_dataset)} evaluation samples")

        # Create tokenizer and model
        progress.update(task, description="Loading tokenizer and model...")
        tokenizer = create_tokenizer(model_name)
        model = create_model(model_name)

        # Tokenize datasets
        progress.update(task, description="Tokenizing datasets...")
        train_dataset = tokenize_dataset(train_dataset, tokenizer)
        if eval_dataset:
            eval_dataset = tokenize_dataset(eval_dataset, tokenizer)

        progress.update(task, description="Setting up training...")

    # Validate early stopping requires eval data
    if early_stopping_patience is not None and eval_dataset is None:
        console.print("[red]Error: --early-stopping requires --eval data[/red]")
        raise typer.Exit(1)

    # Training arguments
    training_args = _create_training_args(
        output_dir, num_epochs, batch_size, learning_rate,
        lr_scheduler_type, warmup_ratio, fp16, eval_dataset, seed
    )

    # Data collator
    data_collator = DataCollatorWithPadding(tokenizer=tokenizer)

    # Setup callbacks
    callbacks = []
    if early_stopping_patience is not None:
        callbacks.append(EarlyStoppingCallback(early_stopping_patience=early_stopping_patience))
        console.print(f"  Early stopping: enabled (patience={early_stopping_patience})")

    # Create trainer
    trainer = Trainer(
        model=model,
        args=training_args,
        train_dataset=train_dataset,
        eval_dataset=eval_dataset,
        tokenizer=tokenizer,
        data_collator=data_collator,
        compute_metrics=compute_metrics if eval_dataset else None,
        callbacks=callbacks if callbacks else None,
    )

    # Train
    _print_training_config(
        model_name, batch_size, learning_rate, lr_scheduler_type,
        warmup_ratio, num_epochs, fp16, early_stopping_patience
    )
    train_result = trainer.train()

    # Save model
    console.print()
    console.print("[bold]Saving model...[/bold]")
    trainer.save_model(str(output_dir / "final"))
    tokenizer.save_pretrained(str(output_dir / "final"))

    # Save training metrics
    metrics = {
        "train_loss": train_result.training_loss,
        "train_runtime": train_result.metrics.get("train_runtime"),
        "train_samples_per_second": train_result.metrics.get(
            "train_samples_per_second"
        ),
    }

    with open(output_dir / "training_metrics.json", "w", encoding="utf-8") as f:
        json.dump(metrics, f, indent=2)

    # Final evaluation if eval data provided
    if eval_dataset:
        _run_final_evaluation(trainer, eval_dataset, output_dir)

    console.print()
    console.print(f"[green]Model saved to: {output_dir / 'final'}[/green]")
    console.print()
    console.print("[dim]Next steps:[/dim]")
    console.print(f"  1. Export to ONNX: python export_onnx.py --model {output_dir / 'final'}")
    console.print("  2. Calibrate: python calibrate.py --model models/intent_classifier.onnx")


@app.command()
def evaluate(
    model_dir: Path = typer.Argument(..., help="Model directory"),
    eval_data: Path = typer.Argument(..., help="Evaluation data JSON file"),
    batch_size: int = typer.Option(32, "--batch-size", help="Batch size"),
) -> None:
    """Evaluate a trained model."""
    console.print("[bold blue]sqry-nl Model Evaluation[/bold blue]")
    console.print()

    # Load model and tokenizer
    tokenizer = AutoTokenizer.from_pretrained(model_dir)
    model = AutoModelForSequenceClassification.from_pretrained(model_dir)

    # Load data
    eval_dataset = load_training_data(eval_data)
    eval_dataset = tokenize_dataset(eval_dataset, tokenizer)

    console.print(f"Loaded {len(eval_dataset)} evaluation samples")
    console.print()

    # Create trainer for evaluation
    training_args = TrainingArguments(
        output_dir="tmp_eval",
        per_device_eval_batch_size=batch_size,
        report_to="none",
    )

    data_collator = DataCollatorWithPadding(tokenizer=tokenizer)

    trainer = Trainer(
        model=model,
        args=training_args,
        tokenizer=tokenizer,
        data_collator=data_collator,
        compute_metrics=compute_metrics,
    )

    # Evaluate
    eval_results = trainer.evaluate(eval_dataset)

    console.print("[bold]Evaluation Results:[/bold]")
    for key, value in sorted(eval_results.items()):
        if isinstance(value, float):
            console.print(f"  {key}: {value:.4f}")
        else:
            console.print(f"  {key}: {value}")

    # Classification report
    console.print()
    console.print("[bold]Classification Report:[/bold]")
    predictions = trainer.predict(eval_dataset)
    preds = np.argmax(predictions.predictions, axis=1)
    labels = predictions.label_ids
    report = classification_report(labels, preds, target_names=INTENT_LABELS, digits=4)
    console.print(report)

    # Cleanup
    import shutil

    shutil.rmtree("tmp_eval", ignore_errors=True)


@app.command()
def predict(
    model_dir: Path = typer.Argument(..., help="Model directory"),
    text: str = typer.Argument(..., help="Text to classify"),
) -> None:
    """Predict intent for a single text."""
    # Load model and tokenizer
    tokenizer = AutoTokenizer.from_pretrained(model_dir)
    model = AutoModelForSequenceClassification.from_pretrained(model_dir)
    model.eval()

    # Tokenize
    inputs = tokenizer(
        text,
        return_tensors="pt",
        truncation=True,
        max_length=MAX_LENGTH,
    )

    # Predict
    with torch.no_grad():
        outputs = model(**inputs)
        logits = outputs.logits
        probs = torch.softmax(logits, dim=-1)
        pred_id = torch.argmax(probs, dim=-1).item()
        confidence = probs[0][pred_id].item()

    predicted_intent = ID2LABEL[pred_id]

    console.print(f"[bold]Input:[/bold] {text}")
    console.print(f"[bold]Predicted Intent:[/bold] {predicted_intent}")
    console.print(f"[bold]Confidence:[/bold] {confidence:.4f}")
    console.print()
    console.print("[bold]All Probabilities:[/bold]")
    for i, label in enumerate(INTENT_LABELS):
        prob = probs[0][i].item()
        probability_bar = "#" * int(prob * 40)
        console.print(f"  {label:15s}: {prob:.4f} {probability_bar}")


if __name__ == "__main__":
    app()
