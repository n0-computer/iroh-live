use moq_media::{list_audio_inputs, list_audio_outputs};

fn main() {
    let inputs = list_audio_inputs();
    println!("=== INPUTS ===");
    for input in inputs {
        println!("{input:?}");
    }

    let outputs = list_audio_outputs();
    println!("=== OUTPUTS ===");
    for output in outputs {
        println!("{output:?}");
    }
}
