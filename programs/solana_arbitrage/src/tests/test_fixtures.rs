use crate::InstructionData;

pub const PUBKEYS_LIST: &[(&str, bool)] = &[
    ("FYnaLRpfVbAi5CnupX1JuxqokiR773WiZPiCz3dzp7BP", true),  // payer [0] w
    ("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA", false),  // token_program [1] r
    ("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb", false),  // token_program_2022 [2] r
    ("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr", true),  // memo [3] w
    ("So11111111111111111111111111111111111111112", false),  // wsol_mint [4] r
    ("Ft6ingqkyR9JkdddhFUhTtKozr2ZbZssA9nu7sPLNtsk", true),  // user_wsol_ata [5] w
    ("495qCc14W5kNGnHvozvMDaVWWbpj6KLZbwX11373pump", false),  // mint [6] r
    ("2Soaf6qjZCcEpvQ92A6WwxUMvCK5XwDTQgwvHqZLjxL8", true),  // user_token_ata [7] w
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
    ("A9AbvoCw63ZxkhbwAkxqNeJfpx8GXkMofQ3mkCGeCWHc", true),  // pump_amm [A9AbvoCw] pool_id [21] w
    ("4AyhAvpbQNfKCMtTP8bGf9V2ZGqaCrJvyXxYxK3r1Vka", true),  // pump_amm [A9AbvoCw] base_vault [22] w
    ("8JhHehecETVhc19MA6tRBxgZWFL1ZsYC3jjEoETxYsam", true),  // pump_amm [A9AbvoCw] quote_vault [23] w
    ("WDKV514AGcLebNdbwrTFvAB1tzHJCUVnkieBiALz15i", true),  // pump_amm [A9AbvoCw] user_volume_acc [24] w
    ("4shRJJF5itY9W29tVSJWVQxxSBmu6ny1BR3X1z5XyqzS", true),  // pump_amm [A9AbvoCw] pool_v2 [25] w
    ("9rj4MDKtdi7Sc6n6eHCME7nhey4J33vzCnLQyG6FJSxT", true),  // pump_amm [A9AbvoCw] user_vol_wsol_ata [26] w
    ("4dMEyLyVk4EckUvkrKJ8QBMyjRFtJ3ccg19wUyUQ8XLS", false),  // pump_amm [A9AbvoCw] vault_ata [27] r
    ("9GdoqLFCU1jsv4roYoZfZUrNanhZq3C9GCsAtpfhqToY", false),  // pump_amm [A9AbvoCw] vault_authority [28] r
    ("2jcYrSUjp1m15GBdgaThK1Rye2u9jHKJQfaymFhahECe", true),  // meteora_dlmm [2jcYrSUj] pool_id [29] w
    ("3asFMVh7NSCrZiuEN5KdFfZ8nboMEVGwMPWwq19DFioG", true),  // meteora_dlmm [2jcYrSUj] base_vault [30] w
    ("y82g6phNLnZKz9UBxATJqCKxGKKK4Q6ogkuyfxVtSZp", true),  // meteora_dlmm [2jcYrSUj] quote_vault [31] w
    ("9X7sStkDJUKaXrWshAtGKUZ7FNAjSeKXAdNAhZqzN9Gp", true),  // meteora_dlmm [2jcYrSUj] oracle [32] w
    ("LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo", true),  // meteora_dlmm [2jcYrSUj] bitmap_ext [33] w
    ("F2jru4ge4YvmtNd77HFYHrgr8nDrzz28gkLyB7EfLAuQ", true),  // meteora_dlmm [2jcYrSUj] bin_array_buy_0 [34] w
    ("F2jru4ge4YvmtNd77HFYHrgr8nDrzz28gkLyB7EfLAuQ", true),  // meteora_dlmm [2jcYrSUj] bin_array_buy_1 [35] w
];

pub fn make_instruction_data(test_mode: bool) -> InstructionData {
    InstructionData {
        mints: 2,
        shared_statics_len: 13,
        pool_types: [9, 3, 0, 0, 0, 0, 0, 0],
        type_static_offsets: [0, 10, 0, 0, 0, 0, 0, 0],
        mode: 0,
        test: false,
        group_sizes: [0, 0, 0, 0],
        pool_fees: vec![12500],
    }
}
