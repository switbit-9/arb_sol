use crate::InstructionData;

pub const PUBKEYS_LIST: &[(&str, bool)] = &[
    ("FYnaLRpfVbAi5CnupX1JuxqokiR773WiZPiCz3dzp7BP", true),  // payer [0] w
    ("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA", false),  // token_program [1] r
    ("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb", false),  // token_program_2022 [2] r
    ("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr", true),  // memo [3] w
    ("So11111111111111111111111111111111111111112", false),  // wsol_mint [4] r
    ("Ft6ingqkyR9JkdddhFUhTtKozr2ZbZssA9nu7sPLNtsk", true),  // user_wsol_ata [5] w
    ("8J69rbLTzWWgUJziFY8jeu5tDwEPBwUz4pKBMr5rpump", false),  // mint [6] r
    ("3yQWhsHgFoCxxQS48TYqhEcsy6br2RyzNR39CJMUdgks", true),  // user_token_ata [7] w
    ("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA", false),  // pump_amm program_id [8] r
    ("62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV", false),  // pump_amm protocol_fee_recipient [9] r
    ("94qWNrtmfn42h3ZjUZwWvK1MEo9uVmmrBPd2hpNjYDjb", true),  // pump_amm protocol_fee_token_acc [10] w
    ("GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR", false),  // pump_amm event_authority [11] r
    ("5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx", false),  // pump_amm fee_config [12] r
    ("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ", false),  // pump_amm fee_program [13] r
    ("ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw", false),  // pump_amm global [14] r
    ("11111111111111111111111111111111", false),  // pump_amm system_program [15] r
    ("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL", false),  // pump_amm assoc_token_prog [16] r
    ("C2aFPdENg4A2HQsmrd5rTw5TaYBX5Ku887cWjbFKtZpw", false),  // pump_amm global_vol_acc [17] r
    ("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo", false),  // meteora_dlmm program_id [18] r
    ("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo", false),  // meteora_dlmm host_fee_in [19] r
    ("D1ZN9Wj1fRSUQfCjhvnu1hqDMT7hzjzBBpi12nVniYD6", false),  // meteora_dlmm event_authority [20] r
    ("FDrY5i5kuadZ1ik8gPS26qjj9Rw9mpufXMegGC2HNSP7", true),  // pump_amm [FDrY5i5k] pool_id [21] w
    ("GUtUvkro8tQsHd8N9urQ3uXGReVZWVqpyrzcwTxPb87", true),  // pump_amm [FDrY5i5k] base_vault [22] w
    ("GEaRRckcM37BuBne4iFy5HBQQntdpFUHbVKsFf7zYj7f", true),  // pump_amm [FDrY5i5k] quote_vault [23] w
    ("WDKV514AGcLebNdbwrTFvAB1tzHJCUVnkieBiALz15i", true),  // pump_amm [FDrY5i5k] user_volume_acc [24] w
    ("4shRJJF5itY9W29tVSJWVQxxSBmu6ny1BR3X1z5XyqzS", true),  // pump_amm [FDrY5i5k] pool_v2 [25] w
    ("3AH2YfHJALgLovXFtfEdJcLAe533c2YfiA2LdwG8pTeJ", true),  // pump_amm [FDrY5i5k] user_vol_wsol_ata [26] w
    ("FWEC1KsaoFS3TmxNgPEURxQsrqepD8s53dbUCwH1GhAo", false),  // pump_amm [FDrY5i5k] vault_ata [27] r
    ("81YExhAkNJk7SCgcFgxHBdHRSBzCAudpaRPbyqyjDRqq", false),  // pump_amm [FDrY5i5k] vault_authority [28] r
    ("2RX1ZogEMsjv1YjMF3m5U1DN7gVCtSLaQHs27F17ECve", true),  // meteora_dlmm [2RX1ZogE] pool_id [29] w
    ("A9PioV2dUhyaNFKvbMiDSf3n5ob1oHWRj5wVN73AJqSB", true),  // meteora_dlmm [2RX1ZogE] base_vault [30] w
    ("D7zWwDfGe9pqeTM9MuTD2zGGh67dwFizKEPTMwSJtBPG", true),  // meteora_dlmm [2RX1ZogE] quote_vault [31] w
    ("7PaFMpxXov7DyNCPiUr5nhfuoCKNWqFuAtMZrhRaR8Y8", true),  // meteora_dlmm [2RX1ZogE] oracle [32] w
    ("HptuEGF2LXvh9W2R1Qx2R5pUVP9qvsFGME5seG5e7GRZ", true),  // meteora_dlmm [2RX1ZogE] bitmap_ext [33] w
    ("Eo8KjwKQAjyLy1VyqeUXpKUhCzRGChhqaq8q9yz6zVC6", true),  // meteora_dlmm [2RX1ZogE] bin_array_buy_0 [34] w
    ("Eo8KjwKQAjyLy1VyqeUXpKUhCzRGChhqaq8q9yz6zVC6", true),  // meteora_dlmm [2RX1ZogE] bin_array_buy_1 [35] w
    ("FJfe9ayR2RbEaoZbJ17f5CB6oQ4cEVoKbq1tehpabYRv", true),  // meteora_dlmm [FJfe9ayR] pool_id [36] w
    ("NNX4TVuW1PaHQhubrpQrV7FXx2YwDz4jWKqzaD8nKps", true),  // meteora_dlmm [FJfe9ayR] base_vault [37] w
    ("Ehh31cB3HwpMe2JEXpWZ9PP2cw4SdNBZs3rAZbnam2jR", true),  // meteora_dlmm [FJfe9ayR] quote_vault [38] w
    ("ABgusGx44ELP6RrDDqgYsoFERc1TxkBdwYPc8vA9Enii", true),  // meteora_dlmm [FJfe9ayR] oracle [39] w
    ("DEaZGZTvtTdUK6GvodzX7CqsMY6QHvNTngu2nvCyGtwM", true),  // meteora_dlmm [FJfe9ayR] bitmap_ext [40] w
    ("3tzjzw4WbW8BsSzfVp1an2759NbRmxabvByZgDmm1yaN", true),  // meteora_dlmm [FJfe9ayR] bin_array_buy_0 [41] w
    ("3tzjzw4WbW8BsSzfVp1an2759NbRmxabvByZgDmm1yaN", true),  // meteora_dlmm [FJfe9ayR] bin_array_buy_1 [42] w
    ("63ChtT5vH7YBnryKQJLUfRgY4ju7McntSq2v5UuHkuZH", true),  // meteora_dlmm [63ChtT5v] pool_id [43] w
    ("J3hcinwttr83GHD6HabpyJzjAw9AUJ2ipqcyd7YjDQao", true),  // meteora_dlmm [63ChtT5v] base_vault [44] w
    ("B92iWUcq9oHoxJBw95WX13KAmP5F2v3WpNtc4ZKhJnX3", true),  // meteora_dlmm [63ChtT5v] quote_vault [45] w
    ("7JMGu7SwZNjTnSuBN85Tyi3cnK5JE6RepVw86NtXCQWZ", true),  // meteora_dlmm [63ChtT5v] oracle [46] w
    ("3Gj2TmS2Cc5WwrX792GEnJpdSgqG2bmHB2Tx58grQeMR", true),  // meteora_dlmm [63ChtT5v] bitmap_ext [47] w
    ("2DYPYGqXtnZczxHkauzMVKyFe7hLn2KA938KbeSajkF4", true),  // meteora_dlmm [63ChtT5v] bin_array_buy_0 [48] w
    ("2DYPYGqXtnZczxHkauzMVKyFe7hLn2KA938KbeSajkF4", true),  // meteora_dlmm [63ChtT5v] bin_array_buy_1 [49] w
    ("DGQJ5K5sf8wv9HTTuTEkKL7xhEF7NwgFRR2vXD1heWwL", true),  // meteora_dlmm [DGQJ5K5s] pool_id [50] w
    ("2xrusdhcirw2xP7DmjUZsZRGMagVuD2dUcoFNPn1ViwK", true),  // meteora_dlmm [DGQJ5K5s] base_vault [51] w
    ("GzkceFikhwYv6vEogNeX51xbzYth6UoEucmdXCEFRu8N", true),  // meteora_dlmm [DGQJ5K5s] quote_vault [52] w
    ("9zYbQaWkpJnvPSxjkNpbEY5wNB8ARrYQXXPx1HHNJHpr", true),  // meteora_dlmm [DGQJ5K5s] oracle [53] w
    ("2YKGRMFwQcUXrBT6jHYwVnnTJA9aZDZoKgzZPWzTJSX7", true),  // meteora_dlmm [DGQJ5K5s] bitmap_ext [54] w
    ("FeizBucRY6Yiyi6tTMHa3Cf8frp5yN5RrkvtYkp1xnrA", true),  // meteora_dlmm [DGQJ5K5s] bin_array_buy_0 [55] w
    ("FeizBucRY6Yiyi6tTMHa3Cf8frp5yN5RrkvtYkp1xnrA", true),  // meteora_dlmm [DGQJ5K5s] bin_array_buy_1 [56] w
];

pub fn make_instruction_data(test_mode: bool) -> InstructionData {
    InstructionData {
        mints: 2,
        shared_statics_len: 13,
        pool_types: [9, 3, 3, 3, 3, 0, 0, 0],
        type_static_offsets: [0, 10, 10, 10, 10, 0, 0, 0],
        mode: 0,
        test: false,
        group_sizes: [0, 0, 0, 0],
        pool_fees: vec![3700],
    }
}
