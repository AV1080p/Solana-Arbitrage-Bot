use crate::common::config::import_env_var;
use anyhow::{anyhow, Result};
use rand::{seq::IteratorRandom, thread_rng};
use anchor_client::solana_sdk::pubkey::Pubkey;
use std::{str::FromStr, sync::LazyLock};

pub static NOZOMI_URL: LazyLock<String> = LazyLock::new(|| import_env_var("NOZOMI_URL"));
pub fn get_nozomi_tip() -> f64 {
    import_env_var("NOZOMI_TIP_VALUE").parse::<f64>().unwrap_or(0.001)
}

pub fn get_tip_account() -> Result<Pubkey> {
    let accounts = [
        "TEMPaMeCRFAS9EKF53Jd6KpHxgL47uWLcpFArU1Fanq".to_string(),
        "noz3jAjPiHuBPqiSPkkugaJDkJscPuRhYnSpbi8UvC4".to_string(),
        "noz3str9KXfpKknefHji8L1mPgimezaiUyCHYMDv1GE".to_string(),
        "noz6uoYCDijhu1V7cutCpwxNiSovEwLdRHPwmgCGDNo".to_string(),
        "noz9EPNcT7WH6Sou3sr3GGjHQYVkN3DNirpbvDkv9YJ".to_string(),
        "nozc5yT15LazbLTFVZzoNZCwjh3yUtW86LoUyqsBu4L".to_string(),
        "nozFrhfnNGoyqwVuwPAW4aaGqempx4PU6g6D9CJMv7Z".to_string(),
        "nozievPk7HyK1Rqy1MPJwVQ7qQg2QoJGyP71oeDwbsu".to_string(),
        "noznbgwYnBLDHu8wcQVCEw6kDrXkPdKkydGJGNXGvL7".to_string(),
        "nozNVWs5N8mgzuD3qigrCG2UoKxZttxzZ85pvAQVrbP".to_string(),
        "nozpEGbwx4BcGp6pvEdAh1JoC2CQGZdU6HbNP1v2p6P".to_string(),
        "nozrhjhkCr3zXT3BiT4WCodYCUFeQvcdUkM7MqhKqge".to_string(),
        "nozrwQtWhEdrA6W8dkbt9gnUaMs52PdAv5byipnadq3".to_string(),
        "nozUacTVWub3cL4mJmGCYjKZTnE9RbdY5AP46iQgbPJ".to_string(),
        "nozWCyTPppJjRuw2fpzDhhWbW355fzosWSzrrMYB1Qk".to_string(),
        "nozWNju6dY353eMkMqURqwQEoM3SFgEKC6psLCSfUne".to_string(),
        "nozxNBgWohjR75vdspfxR5H9ceC7XXH99xpxhVGt3Bb".to_string(),
    ];
    let mut rng = thread_rng();
    let tip_account = match accounts.iter().choose(&mut rng) {
        Some(acc) => Ok(Pubkey::from_str(acc).inspect_err(|err| {
            println!("nozomi: failed to parse Pubkey: {:?}", err);
        })?),
        None => Err(anyhow!("nozomi: no tip accounts available")),
    };

    let tip_account = tip_account?;
    Ok(tip_account)
}


